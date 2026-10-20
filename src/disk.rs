//! On-disk layout and the file primitives that make each step crash safe.
//!
//! Layout (this module is the only one that knows it):
//!
//! ```text
//! <install>/
//!     Zwift_ver_cur.xml              commit point, written last, atomically
//!     <manifest name>                server manifest, verbatim
//!     .zwift-bootstrap/lock          flock: one sync per install
//!     .zwift-bootstrap/pending       name of the manifest a commit is moving toward
//!     .zwift-bootstrap/tmp           cross-filesystem copy target, then rename
//!     .zwift-bootstrap/<name>.tmp    atomic write of a root control file
//!     <manifest paths>               game files
//! <staging>/
//!     lock                           flock: one run per staging dir
//!     meta/Zwift_ver_cur.xml         fresh server ver_cur
//!     meta/<manifest name>           server manifest (reused when its CRC matches)
//!     files/<manifest paths>         verified game files, complete or absent
//!     partial/<worker>               in-flight download, one per worker
//! ```
//!
//! No journal. The state of an interrupted run *is* the content of these files, and
//! every reader re-derives it by length + CRC. That is why nothing here records progress.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::Error;
use crate::model::{
    Checksum, Crc32, FileEntry, MANIFEST_SUFFIX, Manifest, ManifestName, RelPath, VerCur,
};
use crate::parallel::WorkerId;
use crate::wire;

pub(crate) const VER_CUR: &str = "Zwift_ver_cur.xml";
const PRIVATE_DIR: &str = ".zwift-bootstrap";

/// True for manifest paths that would overwrite a file the tool owns in the install root:
/// anything under `.zwift-bootstrap/`, `Zwift_ver_cur.xml`, or a root `*_manifest.xml`.
/// Compared ASCII case-insensitively, because Wine resolves names that way.
pub(crate) fn is_reserved(path: &RelPath) -> bool {
    let first = path.components().next().unwrap_or_default().to_ascii_lowercase();
    let is_root_file = !path.as_str().contains('/');
    first == PRIVATE_DIR
        || (is_root_file && first == VER_CUR.to_ascii_lowercase())
        || (is_root_file && first.ends_with(MANIFEST_SUFFIX))
}

#[derive(Clone, Debug)]
pub(crate) struct Layout {
    pub install: PathBuf,
    pub staging: PathBuf,
}

/// What `inspect` found. Shared by stage-time reuse and by `verify`, so "this file is
/// right" has exactly one definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Inspection {
    Missing,
    WrongLength(u64),
    WrongCrc { expected: Crc32, actual: Crc32 },
    /// Length matches and the entry has no checksum. Good enough for `verify`, never
    /// good enough to skip a download.
    LengthOnly,
    Match,
}

/// The local install as found on disk.
pub(crate) enum LocalState {
    /// No ver_cur, or it or its manifest does not parse. Same as the `0.0.0` seed.
    /// `pending` still applies: a fresh install can crash mid-commit too.
    Empty { reason: String, pending: Pending },
    Installed { ver_cur: VerCur, manifest: Manifest, pending: Pending },
}

/// What an interrupted commit left behind. Read from `.zwift-bootstrap/pending`.
pub(crate) enum Pending {
    /// No marker, or the marker names the manifest the ver_cur already names (the crash
    /// hit between the commit point and the marker removal).
    None,
    /// A commit toward this manifest started and did not reach its commit point.
    Toward(Manifest),
    /// A marker exists but its manifest is missing or invalid. Nothing on disk is
    /// vouched for; `sync` plans against `Manifest::empty()` as pending, so every
    /// target is content-checked.
    Unknown,
}

/// An exclusive advisory lock (std `File::try_lock`). Released on drop and on process
/// death, so a crash never leaves a stale lock.
pub(crate) struct Lock(#[allow(dead_code)] File);

impl Layout {
    pub(crate) fn installed(&self, path: &RelPath) -> PathBuf {
        path.under(&self.install)
    }
    pub(crate) fn staged(&self, path: &RelPath) -> PathBuf {
        path.under(&self.staging.join("files"))
    }
    pub(crate) fn partial(&self, worker: WorkerId) -> PathBuf {
        self.staging.join("partial").join(worker.0.to_string())
    }
    pub(crate) fn staged_meta(&self, name: &str) -> PathBuf {
        self.staging.join("meta").join(name)
    }
    fn private(&self, name: &str) -> PathBuf {
        self.install.join(PRIVATE_DIR).join(name)
    }

    /// Also creates `meta/`, which the control-file downloads write into.
    pub(crate) fn lock_staging(&self) -> Result<Lock, Error> {
        let lock = lock(&self.staging.join("lock"))?;
        let meta = self.staging.join("meta");
        fs::create_dir_all(&meta).map_err(|source| Error::Io { path: meta, source })?;
        Ok(lock)
    }

    pub(crate) fn lock_install(&self) -> Result<Lock, Error> {
        lock(&self.private("lock"))
    }

    /// Read-only. Safe under `check`, which holds no install lock.
    pub(crate) fn load_local(&self) -> LocalState {
        let marker = match fs::read_to_string(self.private("pending")) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(_) => Some(Err(())),
            Ok(name) => Some(ManifestName::parse(name.trim()).map_err(drop)),
        };
        let pending = |current: Option<&ManifestName>| match &marker {
            None => Pending::None,
            Some(Ok(name)) if Some(name) == current => Pending::None,
            Some(Ok(name)) => match self.read_manifest(name, Checksum::Absent) {
                Ok(manifest) => Pending::Toward(manifest),
                Err(_) => Pending::Unknown,
            },
            Some(Err(())) => Pending::Unknown,
        };
        let ver_cur = match fs::read(self.install.join(VER_CUR)) {
            Ok(bytes) => wire::parse_ver_cur(&bytes),
            Err(e) => Err(e.to_string()),
        };
        let ver_cur = match ver_cur {
            Ok(v) => v,
            Err(reason) => return LocalState::Empty { reason: format!("{VER_CUR}: {reason}"), pending: pending(None) },
        };
        match self.read_manifest(&ver_cur.manifest, ver_cur.manifest_checksum) {
            Ok(manifest) => {
                let pending = pending(Some(&ver_cur.manifest));
                LocalState::Installed { ver_cur, manifest, pending }
            }
            Err(reason) => LocalState::Empty {
                reason: format!("{}: {reason}", ver_cur.manifest.as_str()),
                pending: pending(None),
            },
        }
    }

    /// `expect` is the ver_cur's `manifest_checksum`. A mismatch means the file is not
    /// the manifest the ver_cur vouches for (a crash mid-commit to a server version that
    /// reused the manifest name), so it vouches for nothing.
    fn read_manifest(&self, name: &ManifestName, expect: Checksum) -> Result<Manifest, String> {
        let bytes = fs::read(self.install.join(name.as_str())).map_err(|e| e.to_string())?;
        if let Checksum::Crc(want) = expect {
            let actual = Crc32(crc32fast::hash(&bytes));
            if actual != want {
                return Err(format!("checksum {actual}, the ver_cur names {want}"));
            }
        }
        wire::parse_manifest(&bytes)
    }

    /// Start of the commit: record where it is going. Atomic write + fsync.
    pub(crate) fn mark_pending(&self, name: &ManifestName) -> io::Result<()> {
        write_atomic(&self.private("pending"), &self.private("pending.tmp"), name.as_str().as_bytes())
    }

    /// After the commit point. Also removes root `*_manifest.xml` files other than
    /// `keep` (an old manifest a crash left behind). Errors are ignored.
    pub(crate) fn finish_commit(&self, keep: &ManifestName) {
        let _ = fs::remove_file(self.private("pending"));
        let Ok(entries) = fs::read_dir(&self.install) else { return };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.ends_with(MANIFEST_SUFFIX) && name != keep.as_str() {
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    /// Convergent startup: drop in-flight downloads of a dead run. Complete files in
    /// `files/` stay; they are re-verified before use.
    pub(crate) fn reset_partials(&self) -> io::Result<()> {
        let partial = self.staging.join("partial");
        match fs::remove_dir_all(&partial) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        fs::create_dir_all(partial)
    }

    /// Move a verified download from `partial/<worker>` to `files/<path>`. Clears a stale
    /// file or dir from an older staging run that sits where this path or a parent dir
    /// must go.
    pub(crate) fn promote_partial(
        &self,
        worker: WorkerId,
        path: &RelPath,
        modified: Option<SystemTime>,
    ) -> io::Result<()> {
        let partial = self.partial(worker);
        if let Some(time) = modified {
            // The time stamp is cosmetic, so a failure to set it must not fail the file.
            let _ = File::options().write(true).open(&partial).and_then(|f| f.set_modified(time));
        }
        let files = self.staging.join("files");
        let dest = self.staged(path);
        make_room(&files, path)?;
        fs::rename(&partial, &dest)
    }

    /// Move a staged file into the install. Atomic per file: `rename` on one filesystem.
    /// On `CrossesDevices`, copy to `.zwift-bootstrap/tmp` then rename, so a reader never
    /// sees a half-written game file. Creates parent dirs.
    pub(crate) fn place(&self, path: &RelPath) -> io::Result<()> {
        let src = self.staged(path);
        let dest = self.installed(path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        match fs::rename(&src, &dest) {
            Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
                let tmp = self.private("tmp");
                fs::copy(&src, &tmp)?;
                let modified = fs::metadata(&src)?.modified()?;
                File::options().write(true).open(&tmp)?.set_modified(modified)?;
                fs::rename(&tmp, &dest)?;
                fs::remove_file(&src)
            }
            other => other,
        }
    }

    /// Remove an extraneous file, then prune now-empty parent dirs up to the root.
    /// `NotFound` is success (idempotent). Other errors are returned for a warning.
    pub(crate) fn remove(&self, path: &RelPath) -> io::Result<()> {
        match fs::remove_file(self.installed(path)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let dirs: Vec<&str> = path.parent_dirs().collect();
        for dir in dirs.iter().rev() {
            if fs::remove_dir(self.install.join(dir)).is_err() {
                break;
            }
        }
        Ok(())
    }

    /// `syncfs(2)` on the install filesystem: every placed file is durable before the
    /// commit point is. One call instead of an fsync per file.
    pub(crate) fn flush_install(&self) -> io::Result<()> {
        rustix::fs::syncfs(File::open(&self.install)?)?;
        Ok(())
    }

    /// Write `install/<name>` atomically: tmp in the same filesystem, fsync, rename,
    /// fsync dir.
    pub(crate) fn write_install_file(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        write_atomic(&self.install.join(name), &self.private(&format!("{name}.tmp")), bytes)
    }

    /// After success: remove `files/`, `partial/`, `meta/`. Ignores errors.
    pub(crate) fn clear_staging(&self) {
        for dir in ["files", "partial", "meta"] {
            let _ = fs::remove_dir_all(self.staging.join(dir));
        }
    }
}

fn lock(path: &Path) -> Result<Lock, Error> {
    let io_err = |source| Error::Io { path: path.to_owned(), source };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_err)?;
    }
    let file = File::options().create(true).truncate(false).write(true).open(path).map_err(io_err)?;
    match file.try_lock() {
        Ok(()) => Ok(Lock(file)),
        Err(fs::TryLockError::WouldBlock) => Err(Error::Busy(path.to_owned())),
        Err(fs::TryLockError::Error(e)) => Err(io_err(e)),
    }
}

/// Make `root/path` free for a new file: every parent is a dir, the path itself is not a dir.
fn make_room(root: &Path, path: &RelPath) -> io::Result<()> {
    for dir in path.parent_dirs() {
        let dir = root.join(dir);
        if fs::symlink_metadata(&dir).is_ok_and(|m| !m.is_dir()) {
            fs::remove_file(&dir)?;
        }
    }
    let dest = path.under(root);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::symlink_metadata(&dest).is_ok_and(|m| m.is_dir()) {
        fs::remove_dir_all(&dest)?;
    }
    Ok(())
}

fn write_atomic(dest: &Path, tmp: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = tmp.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = File::create(tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(tmp, dest)?;
    if let Some(parent) = dest.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// Stat, then CRC only when the length matches. Streams in 256 KiB blocks.
pub(crate) fn inspect(file: &Path, entry: &FileEntry) -> io::Result<Inspection> {
    let len = match fs::metadata(file) {
        Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => {
            return Ok(Inspection::Missing);
        }
        Err(e) => return Err(e),
        Ok(meta) if !meta.is_file() => return Ok(Inspection::Missing),
        Ok(meta) => meta.len(),
    };
    if len != entry.length {
        return Ok(Inspection::WrongLength(len));
    }
    let Checksum::Crc(expected) = entry.checksum else { return Ok(Inspection::LengthOnly) };
    let actual = crc_file(file)?;
    Ok(if actual == expected { Inspection::Match } else { Inspection::WrongCrc { expected, actual } })
}

pub(crate) fn crc_file(file: &Path) -> io::Result<Crc32> {
    let mut file = File::open(file)?;
    let mut hasher = crc32fast::Hasher::new();
    let mut buf = vec![0; 256 * 1024];
    loop {
        match file.read(&mut buf) {
            Ok(0) => return Ok(Crc32(hasher.finalize())),
            Ok(n) => hasher.update(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> (tempfile::TempDir, Layout) {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout { install: dir.path().join("install"), staging: dir.path().join("staging") };
        fs::create_dir_all(&layout.install).unwrap();
        (dir, layout)
    }

    fn rel(p: &str) -> RelPath {
        RelPath::parse(p).unwrap()
    }

    const MANIFEST: &str = r#"<Zwift version="1.0.1" folder="F"><files/></Zwift>"#;

    fn ver_cur(manifest: &str, checksum: &str) -> String {
        format!(r#"<Zwift version="1.0.1" manifest="{manifest}" manifest_checksum="{checksum}"/>"#)
    }

    #[test]
    fn remove_prunes_empty_parents_and_treats_missing_as_done() {
        let (_dir, layout) = layout();
        fs::create_dir_all(layout.install.join("a/b")).unwrap();
        fs::write(layout.install.join("a/b/c"), "x").unwrap();
        fs::write(layout.install.join("a/keep"), "x").unwrap();
        layout.remove(&rel("a/b/c")).unwrap();
        assert!(!layout.install.join("a/b").exists());
        assert!(layout.install.join("a/keep").exists());
        layout.remove(&rel("a/b/c")).unwrap();
    }

    #[test]
    fn promote_partial_replaces_stale_staging_entries_in_the_way() {
        let (_dir, layout) = layout();
        layout.reset_partials().unwrap();
        fs::create_dir_all(layout.staging.join("files/a/b")).unwrap();
        fs::write(layout.staging.join("files/a/b/old"), "x").unwrap();
        fs::write(layout.partial(WorkerId(0)), "new").unwrap();
        layout.promote_partial(WorkerId(0), &rel("a/b"), None).unwrap();
        assert_eq!(fs::read(layout.staged(&rel("a/b"))).unwrap(), b"new");

        fs::write(layout.partial(WorkerId(0)), "deeper").unwrap();
        layout.promote_partial(WorkerId(0), &rel("a/b/c"), None).unwrap();
        assert_eq!(fs::read(layout.staged(&rel("a/b/c"))).unwrap(), b"deeper");
    }

    #[test]
    fn inspect_reads_the_crc_only_when_the_length_matches() {
        let (_dir, layout) = layout();
        let file = layout.install.join("f");
        fs::write(&file, "hello").unwrap();
        let entry = |length, checksum| FileEntry { length, checksum, modified: None };
        let crc = Crc32(crc32fast::hash(b"hello"));
        assert_eq!(inspect(&file, &entry(5, Checksum::Crc(crc))).unwrap(), Inspection::Match);
        assert_eq!(inspect(&file, &entry(5, Checksum::Absent)).unwrap(), Inspection::LengthOnly);
        assert_eq!(inspect(&file, &entry(4, Checksum::Crc(crc))).unwrap(), Inspection::WrongLength(5));
        assert_eq!(
            inspect(&file, &entry(5, Checksum::Crc(Crc32(1)))).unwrap(),
            Inspection::WrongCrc { expected: Crc32(1), actual: crc }
        );
        let under_file = file.join("x");
        assert_eq!(inspect(&under_file, &entry(5, Checksum::Absent)).unwrap(), Inspection::Missing);
    }

    #[test]
    fn load_local_reads_the_pending_marker_states() {
        let (_dir, layout) = layout();
        let crc = Crc32(crc32fast::hash(MANIFEST.as_bytes()));
        fs::write(layout.install.join("a_manifest.xml"), MANIFEST).unwrap();
        fs::write(layout.install.join(VER_CUR), ver_cur("a_manifest.xml", &crc.to_string())).unwrap();
        let pending = |layout: &Layout| match layout.load_local() {
            LocalState::Installed { pending, .. } => pending,
            LocalState::Empty { reason, .. } => panic!("not installed: {reason}"),
        };
        assert!(matches!(pending(&layout), Pending::None));

        layout.mark_pending(&ManifestName::parse("a_manifest.xml").unwrap()).unwrap();
        assert!(matches!(pending(&layout), Pending::None), "a marker naming the current manifest is stale");

        layout.mark_pending(&ManifestName::parse("b_manifest.xml").unwrap()).unwrap();
        assert!(matches!(pending(&layout), Pending::Unknown), "marker names a missing manifest");

        fs::write(layout.install.join("b_manifest.xml"), MANIFEST).unwrap();
        assert!(matches!(pending(&layout), Pending::Toward(_)));

        layout.finish_commit(&ManifestName::parse("a_manifest.xml").unwrap());
        assert!(matches!(pending(&layout), Pending::None));
        assert!(!layout.install.join("b_manifest.xml").exists());
    }

    #[test]
    fn local_manifest_that_fails_the_ver_cur_checksum_vouches_for_nothing() {
        let (_dir, layout) = layout();
        fs::write(layout.install.join("a_manifest.xml"), MANIFEST).unwrap();
        fs::write(layout.install.join(VER_CUR), ver_cur("a_manifest.xml", "12345")).unwrap();
        assert!(matches!(layout.load_local(), LocalState::Empty { .. }));
    }

    #[test]
    fn second_lock_on_the_same_dir_is_busy() {
        let (_dir, layout) = layout();
        let _held = layout.lock_install().unwrap();
        assert!(matches!(layout.lock_install(), Err(Error::Busy(_))));
    }
}
