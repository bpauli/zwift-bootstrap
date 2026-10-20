//! Keep the Zwift game files in a directory equal to what the Zwift CDN publishes.
//!
//! ```no_run
//! use zwift_bootstrap::{Bootstrapper, Config, SyncMode};
//! let bs = Bootstrapper::new(Config {
//!     install_dir: "/home/me/.local/share/zwift".into(),
//!     staging_dir: "/home/me/.cache/zwift/downloads".into(),
//!     base_url: zwift_bootstrap::DEFAULT_BASE_URL.into(),
//!     jobs: 8.try_into().unwrap(),
//! })?;
//! let outcome = bs.sync(SyncMode::Install, &mut |event| eprintln!("{event:?}"))?;
//! # Ok::<(), zwift_bootstrap::Error>(())
//! ```
//!
//! Module map. Each module owns one body of knowledge:
//! - `model`    what valid protocol data is (paths, versions, checksums, manifests)
//! - `wire`     XML syntax -> `model` (the untrusted-input boundary)
//! - `plan`     pure decisions: is there an update, what differs
//! - `disk`     the on-disk layout and crash-safe file primitives
//! - `net`      the CDN: one verified, retrying fetch
//! - `parallel` the worker pool and the single progress owner
//! - `sync`     `check` and `sync`: the pipeline and its commit order
//! - this file  the public surface, `verify`, `launch`

mod disk;
mod model;
mod net;
mod parallel;
mod plan;
mod sync;
mod wire;

use std::convert::Infallible;
use std::ffi::{OsStr, OsString};
use std::num::NonZeroUsize;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use disk::{Inspection, LocalState, Pending};

pub use model::{Checksum, Crc32, PathRejection, RelPath, Version};

pub const DEFAULT_BASE_URL: &str = "https://cdn.zwift.com/gameassets/Zwift_Updates_Root/";
pub const LAUNCHER_VERSION: &str = "1.2.0";

pub struct Config {
    pub install_dir: PathBuf,
    pub staging_dir: PathBuf,
    pub base_url: String,
    pub jobs: NonZeroUsize,
}

pub struct Bootstrapper {
    layout: disk::Layout,
    remote: net::Remote,
    jobs: NonZeroUsize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncMode {
    /// No valid local state is the `0.0.0` seed: download everything.
    Install,
    /// No valid local state is `Error::NotInstalled`.
    Update,
}

/// Result of `check`. The variants carry exactly what each case has.
#[derive(Debug)]
pub enum Status {
    Current { version: Version },
    UpdateAvailable {
        /// `None`: no valid install in the directory.
        local: Option<Version>,
        server: Version,
        mandatory: bool,
        plan: PlanSummary,
    },
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PlanSummary {
    pub fetch_files: usize,
    pub fetch_bytes: u64,
    pub delete_files: usize,
}

#[derive(Debug)]
pub enum SyncOutcome {
    Current { version: Version },
    Updated { from: Option<Version>, to: Version, report: SyncReport },
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SyncReport {
    pub downloaded_files: usize,
    pub downloaded_bytes: u64,
    /// Found already correct in staging or in the install (resume after a crash).
    pub reused_files: usize,
    pub deleted_files: usize,
}

#[derive(Debug)]
pub struct VerifyReport {
    pub version: Version,
    pub checked: usize,
    pub problems: Vec<Problem>,
}

#[derive(Debug)]
pub struct Problem {
    pub path: RelPath,
    pub kind: ProblemKind,
}

#[derive(Debug)]
pub enum ProblemKind {
    Missing,
    WrongLength { expected: u64, actual: u64 },
    WrongCrc { expected: Crc32, actual: Crc32 },
}

/// Progress for a renderer. Delivered on the caller's thread only.
#[derive(Debug, Clone)]
pub enum Event {
    Phase(Phase),
    /// Snapshot, at most every 100 ms. The renderer keeps no state.
    Progress(Tally),
    Retry { path: RelPath, attempt: u32, cause: String },
    /// Non-fatal (an extraneous file that would not delete).
    Warning(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    CheckingVersion,
    FetchingManifest,
    Staging,
    Committing,
    Verifying,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Tally {
    pub files_done: usize,
    pub files_total: usize,
    /// Downloaded plus reused bytes, so the bar reaches 100%.
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// Network bytes only, for a rate.
    pub bytes_downloaded: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("another zwift-bootstrap run holds {0}")]
    Busy(PathBuf),
    #[error("{dir} has no valid Zwift install ({reason}); run `install` first")]
    NotInstalled { dir: PathBuf, reason: String },
    #[error("invalid base URL {0:?}")]
    BaseUrl(String),
    #[error("server sent an invalid {what}: {reason}")]
    BadServerData { what: &'static str, reason: String },
    #[error("manifest checksum mismatch: expected {expected}, got {actual}")]
    ManifestChecksum { expected: Crc32, actual: Crc32 },
    #[error("{url}: {cause}")]
    Download { url: String, cause: String },
    #[error("{} files failed to download; the install is unchanged, rerun to resume", .0.len())]
    Files(Vec<(RelPath, String)>),
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
}

impl Bootstrapper {
    pub fn new(config: Config) -> Result<Bootstrapper, Error> {
        Ok(Bootstrapper {
            layout: disk::Layout { install: config.install_dir, staging: config.staging_dir },
            remote: net::Remote::new(&config.base_url)?,
            jobs: config.jobs,
        })
    }

    // `check` and `sync` live in sync.rs (same type, separate impl block).

    /// Read every file the local manifest lists. Never writes. Never touches the network.
    pub fn verify(&self, on_event: &mut dyn FnMut(Event)) -> Result<VerifyReport, Error> {
        let (ver_cur, manifest, pending) = match self.layout.load_local() {
            LocalState::Installed { ver_cur, manifest, pending } => (ver_cur, manifest, pending),
            LocalState::Empty { reason, .. } => return Err(self.not_installed(reason)),
        };
        if !matches!(pending, Pending::None) {
            on_event(Event::Warning("an update was interrupted; run `update` to finish it".into()));
        }
        on_event(Event::Phase(Phase::Verifying));
        let items: Vec<_> = manifest.files().iter().collect();
        let tally = Tally {
            files_total: items.len(),
            bytes_total: items.iter().map(|(_, e)| e.length).sum(),
            ..Tally::default()
        };
        let results = parallel::for_each(&items, self.jobs, tally, on_event, |(path, entry), sink| {
            let inspection = disk::inspect(&self.layout.installed(path), entry);
            sink.file_done(false, entry.length);
            inspection
        });
        let mut problems = Vec::new();
        for ((path, entry), inspection) in items.iter().zip(results) {
            let inspection = inspection.map_err(sync::io_error(&self.layout.installed(path)))?;
            let kind = match inspection {
                Inspection::Match | Inspection::LengthOnly => continue,
                Inspection::Missing => ProblemKind::Missing,
                Inspection::WrongLength(actual) => ProblemKind::WrongLength { expected: entry.length, actual },
                Inspection::WrongCrc { expected, actual } => ProblemKind::WrongCrc { expected, actual },
            };
            problems.push(Problem { path: (*path).clone(), kind });
        }
        Ok(VerifyReport { version: ver_cur.version, checked: items.len(), problems })
    }

    /// Replace this process with `$WINE ZwiftApp.exe --launcher_version=1.2.0 --token=
    /// EXTRA...`, working dir = install dir. Prefers `ZwiftAppDev.exe` when present.
    /// Refuses when there is no valid install.
    pub fn launch(&self, wine: &OsStr, wine_prefix: &Path, extra: &[OsString]) -> Result<Infallible, Error> {
        match self.layout.load_local() {
            LocalState::Empty { reason, .. } => return Err(self.not_installed(reason)),
            LocalState::Installed { ver_cur, .. } if ver_cur.version == Version::ZERO => {
                return Err(self.not_installed("version 0.0.0 has no game files".into()));
            }
            LocalState::Installed { pending: Pending::Toward(_) | Pending::Unknown, .. } => {
                return Err(self.not_installed("an update was interrupted".into()));
            }
            LocalState::Installed { .. } => {}
        }
        let exe = if self.layout.install.join("ZwiftAppDev.exe").is_file() {
            "ZwiftAppDev.exe"
        } else {
            "ZwiftApp.exe"
        };
        let error = Command::new(wine)
            .arg(exe)
            .arg(format!("--launcher_version={LAUNCHER_VERSION}"))
            .arg("--token=")
            .args(extra)
            .env("WINEPREFIX", wine_prefix)
            .current_dir(&self.layout.install)
            .exec();
        Err(Error::Io { path: PathBuf::from(wine), source: error })
    }

    fn not_installed(&self, reason: String) -> Error {
        Error::NotInstalled { dir: self.layout.install.clone(), reason }
    }
}
