//! `check` and `sync`: the pipeline and its commit order.
//!
//! The pipeline is a typestate. Each step consumes the previous proof:
//!
//! ```text
//! LocalState --assess--> Prepared::Current
//!                    \-> Planned --stage--> Staged --commit--> SyncReport
//! ```
//!
//! `Staged` has private fields and only `stage` builds it, and only when every target
//! is verified. `commit` takes it by value. So "touch the install before every byte is
//! verified" does not compile from outside this module.
//!
//! Crash safety without a progress journal. Every run re-derives where it is from file
//! content. The one piece of recorded state is the pending marker, which says only
//! "the old manifest no longer vouches for the disk".
//!
//! | crash point                  | on disk after crash                     | next run                                   |
//! |------------------------------|-----------------------------------------|--------------------------------------------|
//! | during stage                 | install untouched; some files/ complete | same plan; staged files pass CRC, reused   |
//! | after new manifest written   | extra root manifest file, no marker     | same plan; commit rewrites it              |
//! | after marker, during delete  | some extraneous gone                    | diff(local, pending, server): delete again,|
//! | or place                     | some targets in install, rest in files/ | NotFound is ok; placed targets InPlace     |
//! | same, server moved to C      | install mixes A and B                   | pending=B un-vouches B-touched paths; C    |
//! |                              |                                         | targets content-checked, B-only deleted    |
//! | same, server stayed at A     | install mixes A and B, versions equal   | marker forces a diff (never Current);      |
//! |                              |                                         | converges back to A the same way           |
//! | after ver_cur, before finish | committed, stale marker, old manifest   | marker ignored (names current); finish     |
//! |                              |                                         | runs on the next sync, Current included    |
//!
//! The new manifest and the marker go first, and the old manifest is deleted after the
//! ver_cur write, so the local ver_cur always names a manifest that exists.

use std::fs;
use std::io;
use std::path::Path;

use crate::disk::{self, Inspection, LocalState, Pending};
use crate::model::{Manifest, VerCur, Version};
use crate::net::{Expect, Size};
use crate::parallel::{self, Sink};
use crate::plan::{self, Assessment, Plan, Target};
use crate::{Bootstrapper, Error, Event, Phase, Status, SyncMode, SyncOutcome, SyncReport, Tally};

enum Prepared {
    Current(Version),
    Update(Planned),
}

struct Planned {
    server_ver_cur: VerCur,
    server_manifest: Manifest,
    plan: Plan,
    mandatory: bool,
}

/// Proof that every `plan.fetch` target is correct either in the install already or in
/// `staging/files`. Only `stage` constructs it.
struct Staged {
    planned: Planned,
    /// Targets whose verified copy is in `staging/files` and must be placed.
    to_place: Vec<Target>,
    report: SyncReport,
}

/// Per-target outcome of the stage step.
enum Ready {
    /// The install already has the target content (a crash mid-place).
    InPlace,
    /// `staging/files` has it. `downloaded` is false when a prior run left it there.
    Staged { downloaded: bool },
}

impl Bootstrapper {
    /// Version check and diff. Writes only inside the staging dir (meta/).
    pub fn check(&self, on_event: &mut dyn FnMut(Event)) -> Result<Status, Error> {
        let _staging = self.layout.lock_staging()?;
        let local = self.layout.load_local();
        let local_version = installed_version(&local);
        Ok(match self.prepare(&local, on_event)? {
            Prepared::Current(version) => Status::Current { version },
            Prepared::Update(planned) => Status::UpdateAvailable {
                local: local_version,
                server: planned.server_ver_cur.version,
                mandatory: planned.mandatory,
                plan: planned.plan.summary(),
            },
        })
    }

    /// `install` and `update`. Converges from any interrupted earlier run.
    pub fn sync(&self, mode: SyncMode, on_event: &mut dyn FnMut(Event)) -> Result<SyncOutcome, Error> {
        let _install = self.layout.lock_install()?;
        let _staging = self.layout.lock_staging()?;
        let local = self.layout.load_local();
        if let (SyncMode::Update, LocalState::Empty { reason, .. }) = (mode, &local) {
            return Err(Error::NotInstalled { dir: self.layout.install.clone(), reason: reason.clone() });
        }
        match self.prepare(&local, on_event)? {
            Prepared::Current(version) => {
                if let LocalState::Installed { ver_cur, .. } = &local {
                    self.layout.finish_commit(&ver_cur.manifest);
                }
                Ok(SyncOutcome::Current { version })
            }
            Prepared::Update(planned) => {
                let staged = self.stage(planned, on_event)?;
                let (to, report) = self.commit(staged, on_event)?;
                Ok(SyncOutcome::Updated { from: installed_version(&local), to, report })
            }
        }
    }

    fn prepare(&self, local: &LocalState, on_event: &mut dyn FnMut(Event)) -> Result<Prepared, Error> {
        on_event(Event::Phase(Phase::CheckingVersion));
        let detached = Sink::detached();
        let server_ver_cur = self.remote.fetch_ver_cur(&self.layout.staged_meta(disk::VER_CUR), &detached)?;
        let empty = Manifest::empty();
        let (local_version, local_manifest, pending) = match local {
            LocalState::Empty { pending, .. } => (Version::ZERO, &empty, pending),
            LocalState::Installed { ver_cur, manifest, pending } => (ver_cur.version, manifest, pending),
        };
        let pending = match pending {
            Pending::None => None,
            Pending::Toward(manifest) => Some(manifest),
            Pending::Unknown => Some(&empty),
        };
        let mandatory = match plan::assess(local_version, pending.is_some(), &server_ver_cur) {
            Assessment::Current => return Ok(Prepared::Current(server_ver_cur.version)),
            Assessment::Update { mandatory } => mandatory,
        };
        on_event(Event::Phase(Phase::FetchingManifest));
        let manifest_dest = self.layout.staged_meta(server_ver_cur.manifest.as_str());
        let server_manifest = self.remote.fetch_manifest(&server_ver_cur, &manifest_dest, &detached)?;
        let plan = plan::diff(local_manifest, pending, &server_manifest);
        Ok(Prepared::Update(Planned { server_ver_cur, server_manifest, plan, mandatory }))
    }

    /// Make every fetch target correct in the install or in staging. Never writes the install.
    fn stage(&self, planned: Planned, on_event: &mut dyn FnMut(Event)) -> Result<Staged, Error> {
        self.layout.reset_partials().map_err(io_error(&self.layout.staging))?;
        on_event(Event::Phase(Phase::Staging));
        let fetch = &planned.plan.fetch;
        let tally = Tally {
            files_total: fetch.len(),
            bytes_total: fetch.iter().map(|t| t.entry.length).sum(),
            ..Tally::default()
        };
        let results = parallel::for_each(fetch, self.jobs, tally, on_event, |target, sink| {
            self.ready(&planned, target, sink)
        });
        let mut failures = Vec::new();
        let mut to_place = Vec::new();
        let mut report = SyncReport::default();
        for (target, result) in fetch.iter().zip(results) {
            match result {
                Ok(Ready::InPlace) => report.reused_files += 1,
                Ok(Ready::Staged { downloaded }) => {
                    if downloaded {
                        report.downloaded_files += 1;
                        report.downloaded_bytes += target.entry.length;
                    } else {
                        report.reused_files += 1;
                    }
                    to_place.push(target.clone());
                }
                Err(cause) => failures.push((target.path.clone(), cause)),
            }
        }
        if !failures.is_empty() {
            return Err(Error::Files(failures));
        }
        Ok(Staged { planned, to_place, report })
    }

    /// One target, decided by content in this order: install, staging, network.
    fn ready(&self, planned: &Planned, target: &Target, sink: &Sink) -> Result<Ready, String> {
        let Target { path, entry } = target;
        if let Ok(Inspection::Match) = disk::inspect(&self.layout.installed(path), entry) {
            sink.file_done(false, entry.length);
            return Ok(Ready::InPlace);
        }
        if let Ok(Inspection::Match) = disk::inspect(&self.layout.staged(path), entry) {
            sink.file_done(false, entry.length);
            return Ok(Ready::Staged { downloaded: false });
        }
        let url = self.remote.file_url(planned.server_manifest.folder(), path);
        let expect = Expect { size: Size::Exact(entry.length), checksum: entry.checksum };
        let on_retry = |attempt, cause: &_| sink.retry(path.clone(), attempt, format!("{cause}"));
        self.remote
            .fetch(&url, &self.layout.partial(sink.worker()), expect, sink, &on_retry)
            .map_err(|failure| failure.cause.to_string())?;
        self.layout
            .promote_partial(sink.worker(), path, entry.modified)
            .map_err(|e| format!("staging {path}: {e}"))?;
        sink.file_done(true, entry.length);
        Ok(Ready::Staged { downloaded: true })
    }

    /// The only code that mutates the install. Order is the crash-safety contract above.
    fn commit(&self, staged: Staged, on_event: &mut dyn FnMut(Event)) -> Result<(Version, SyncReport), Error> {
        on_event(Event::Phase(Phase::Committing));
        let Staged { planned, to_place, mut report } = staged;
        let layout = &self.layout;
        let install_err = io_error(&layout.install);
        let name = &planned.server_ver_cur.manifest;
        let manifest_src = layout.staged_meta(name.as_str());
        let manifest_bytes = fs::read(&manifest_src).map_err(io_error(&manifest_src))?;
        layout.write_install_file(name.as_str(), &manifest_bytes).map_err(&install_err)?;
        layout.mark_pending(name).map_err(&install_err)?;
        // Deletes go before placing, so a new file can take the name of a dir that left.
        for path in &planned.plan.delete {
            match layout.remove(path) {
                Ok(()) => report.deleted_files += 1,
                Err(e) => on_event(Event::Warning(format!("could not delete {path}: {e}"))),
            }
        }
        for target in &to_place {
            let dest = layout.installed(&target.path);
            layout.place(&target.path).map_err(io_error(&dest))?;
        }
        layout.flush_install().map_err(&install_err)?;
        layout.write_install_file(disk::VER_CUR, planned.server_ver_cur.raw()).map_err(&install_err)?;
        layout.finish_commit(name);
        layout.clear_staging();
        Ok((planned.server_ver_cur.version, report))
    }
}

fn installed_version(local: &LocalState) -> Option<Version> {
    match local {
        LocalState::Empty { .. } => None,
        LocalState::Installed { ver_cur, .. } => Some(ver_cur.version),
    }
}

pub(crate) fn io_error(path: &Path) -> impl Fn(io::Error) -> Error + '_ {
    move |source| Error::Io { path: path.to_owned(), source }
}
