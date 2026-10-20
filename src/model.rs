//! Domain types. Each value is valid by construction. Code that holds one trusts it.
//!
//! This module owns the *meaning* of the protocol data (what a valid path, version,
//! checksum, or manifest is). `wire.rs` owns only the XML syntax and calls in here.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// `major.minor.build`. Ordered for `mandatory_version`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub build: u32,
}

impl Version {
    /// The seed version of an empty install.
    pub const ZERO: Version = Version { major: 0, minor: 0, build: 0 };

    pub(crate) fn parse(text: &str) -> Result<Version, String> {
        let bad = || format!("invalid version {text:?}");
        let mut parts = text.split('.').map(|part| {
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return Err(bad());
            }
            part.parse::<u32>().map_err(|_| bad())
        });
        let (Some(major), Some(minor), Some(build), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(bad());
        };
        Ok(Version { major: major?, minor: minor?, build: build? })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.build)
    }
}

/// A CRC-32 (IEEE, reflected 0xEDB88320). Wire form is signed 32-bit decimal.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Crc32(pub u32);

impl fmt::Display for Crc32 {
    /// Signed decimal, the way the manifest prints it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0 as i32)
    }
}

/// The text `-1` means "not provided". That is a protocol rule, so it is a variant,
/// not a magic value inside `Crc32`. (A real CRC of 0xFFFFFFFF is indistinguishable
/// on the wire; the protocol accepts that, and so do we.)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Checksum {
    Crc(Crc32),
    Absent,
}

impl Checksum {
    pub(crate) fn parse(text: &str) -> Result<Checksum, String> {
        match text.trim().parse::<i32>() {
            Ok(-1) => Ok(Checksum::Absent),
            Ok(v) => Ok(Checksum::Crc(Crc32(v as u32))),
            Err(_) => Err(format!("invalid checksum {text:?}")),
        }
    }
}

/// A manifest path that is safe to join under the install root and to put in a URL.
///
/// Invariant: non-empty, relative, components joined by `/`, no component is empty,
/// `.`, or `..`, no component has `:` (drive letters, NTFS streams), NUL, or control
/// characters, total length <= 4096. Both `\` and `/` in the raw text count as
/// separators, so a `/` cannot smuggle a `..` past the component check.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct RelPath(Box<str>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathRejection {
    Empty,
    Absolute,
    EmptyComponent,
    CurrentDir,
    ParentDir,
    Colon,
    ControlChar,
    TooLong,
}

impl RelPath {
    pub fn parse(raw: &str) -> Result<RelPath, PathRejection> {
        if raw.is_empty() {
            return Err(PathRejection::Empty);
        }
        if raw.len() > 4096 {
            return Err(PathRejection::TooLong);
        }
        if raw.starts_with(['\\', '/']) {
            return Err(PathRejection::Absolute);
        }
        let mut components = Vec::new();
        for component in raw.split(['\\', '/']) {
            match component {
                "" => return Err(PathRejection::EmptyComponent),
                "." => return Err(PathRejection::CurrentDir),
                ".." => return Err(PathRejection::ParentDir),
                c if c.contains(':') => return Err(PathRejection::Colon),
                c if c.chars().any(char::is_control) => return Err(PathRejection::ControlChar),
                c => components.push(c),
            }
        }
        Ok(RelPath(components.join("/").into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }

    /// Every proper ancestor directory, as a `/`-joined prefix: `a/b/c` -> `a`, `a/b`.
    pub(crate) fn parent_dirs(&self) -> impl Iterator<Item = &str> {
        self.0.match_indices('/').map(|(i, _)| &self.0[..i])
    }

    pub(crate) fn under(&self, root: &Path) -> PathBuf {
        root.join(&*self.0)
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// `[A-Za-z0-9._-]+`, not `.` or `..`. Safe as one URL segment and one file name.
fn check_segment(text: &str) -> Result<(), String> {
    let allowed = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-');
    if text.is_empty() || text == "." || text == ".." || !text.bytes().all(allowed) {
        return Err(format!("invalid name {text:?}"));
    }
    Ok(())
}

/// The manifest `folder` attribute. A server URL segment only, never a local folder.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct ServerFolder(Box<str>);

impl ServerFolder {
    pub(crate) fn parse(text: &str) -> Result<ServerFolder, String> {
        check_segment(text).map(|()| ServerFolder(text.into()))
    }
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

pub(crate) const MANIFEST_SUFFIX: &str = "_manifest.xml";

/// The ver_cur `manifest` attribute. One file name, used in the URL and in the install root.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct ManifestName(Box<str>);

impl ManifestName {
    /// The `_manifest.xml` suffix keeps the name inside the root names that
    /// `disk::is_reserved` keeps away from game files, so the manifest can neither
    /// overwrite a game file nor the ver_cur commit point.
    pub(crate) fn parse(text: &str) -> Result<ManifestName, String> {
        check_segment(text)?;
        if !text.ends_with(MANIFEST_SUFFIX) {
            return Err(format!("manifest name {text:?} does not end with {MANIFEST_SUFFIX}"));
        }
        Ok(ManifestName(text.into()))
    }
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug)]
pub(crate) struct FileEntry {
    pub length: u64,
    pub checksum: Checksum,
    /// From `<modified>`, read as UTC (the protocol does not know the zone). `None` when
    /// absent or malformed: it is cosmetic, so it never fails an install.
    pub modified: Option<SystemTime>,
}

/// A parsed server or local manifest.
///
/// Invariants (enforced by `Manifest::new`, the only constructor besides `empty`):
/// paths are unique; no two paths are equal after ASCII case folding (Wine resolves
/// case-insensitively); no path is also the parent directory of another path; no path
/// collides with a name the tool owns in the install root (`disk::is_reserved`).
#[derive(Clone, Debug)]
pub(crate) struct Manifest {
    version: Version,
    folder: ServerFolder,
    files: BTreeMap<RelPath, FileEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ManifestRejection {
    Duplicate(RelPath),
    CaseCollision(RelPath, RelPath),
    FileIsAlsoDir(RelPath),
    Reserved(RelPath),
}

impl Manifest {
    /// The `0.0.0` seed: no files. Diffing against it fetches everything.
    pub(crate) fn empty() -> Manifest {
        Manifest {
            version: Version::ZERO,
            folder: ServerFolder("Zwift_0.0.0".into()),
            files: BTreeMap::new(),
        }
    }

    pub(crate) fn new(
        version: Version,
        folder: ServerFolder,
        entries: Vec<(RelPath, FileEntry)>,
    ) -> Result<Manifest, ManifestRejection> {
        let mut files = BTreeMap::new();
        let mut folded: HashMap<String, RelPath> = HashMap::new();
        for (path, entry) in entries {
            if crate::disk::is_reserved(&path) {
                return Err(ManifestRejection::Reserved(path));
            }
            if let Some(other) = folded.insert(path.as_str().to_ascii_lowercase(), path.clone()) {
                return Err(if other == path {
                    ManifestRejection::Duplicate(path)
                } else {
                    ManifestRejection::CaseCollision(other, path)
                });
            }
            files.insert(path, entry);
        }
        // Folded, because under Wine `A` the file and `a/b` the dir are the same name.
        let dirs: HashSet<String> = files
            .keys()
            .flat_map(|p| p.parent_dirs().map(str::to_ascii_lowercase))
            .collect();
        if let Some(path) = files.keys().find(|p| dirs.contains(&p.as_str().to_ascii_lowercase())) {
            return Err(ManifestRejection::FileIsAlsoDir(path.clone()));
        }
        Ok(Manifest { version, folder, files })
    }

    pub(crate) fn version(&self) -> Version {
        self.version
    }
    pub(crate) fn folder(&self) -> &ServerFolder {
        &self.folder
    }
    pub(crate) fn files(&self) -> &BTreeMap<RelPath, FileEntry> {
        &self.files
    }
}

/// The parsed `Zwift_ver_cur.xml`. Keeps the exact served bytes, so the install gets the
/// server copy verbatim (CRLF and `ver_cur_checksum` intact) without a serializer.
#[derive(Clone, Debug)]
pub(crate) struct VerCur {
    pub version: Version,
    pub manifest: ManifestName,
    pub manifest_checksum: Checksum,
    pub mandatory_version: Option<Version>,
    raw: Box<[u8]>,
}

impl VerCur {
    pub(crate) fn new(
        version: Version,
        manifest: ManifestName,
        manifest_checksum: Checksum,
        mandatory_version: Option<Version>,
        raw: Box<[u8]>,
    ) -> VerCur {
        VerCur { version, manifest, manifest_checksum, mandatory_version, raw }
    }
    pub(crate) fn raw(&self) -> &[u8] {
        &self.raw
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> FileEntry {
        FileEntry { length: 1, checksum: Checksum::Absent, modified: None }
    }

    fn manifest(paths: &[&str]) -> Result<Manifest, ManifestRejection> {
        let entries = paths.iter().map(|p| (RelPath::parse(p).unwrap(), entry())).collect();
        Manifest::new(Version::ZERO, ServerFolder::parse("Zwift_0.0.0").unwrap(), entries)
    }

    fn rel(p: &str) -> RelPath {
        RelPath::parse(p).unwrap()
    }

    #[test]
    fn rel_path_accepts_backslash_paths_and_joins_with_slash() {
        for (raw, joined) in [
            (r"data\Worlds\world9\map04.ztx", "data/Worlds/world9/map04.ztx"),
            ("ZwiftApp.exe", "ZwiftApp.exe"),
            (r"assets\a/b c.bin", "assets/a/b c.bin"),
            ("..a", "..a"),
        ] {
            let parsed = RelPath::parse(raw).map(|p| p.as_str().to_owned());
            assert_eq!(parsed, Ok(joined.to_owned()), "{raw:?}");
        }
    }

    #[test]
    fn rel_path_rejects_every_hostile_form() {
        use PathRejection::*;
        let long = "a".repeat(4097);
        for (raw, why) in [
            ("", Empty),
            (r"\abs", Absolute),
            ("/abs", Absolute),
            (r"..\x", ParentDir),
            (r"a\..\b", ParentDir),
            ("a/../b", ParentDir),
            (r"a\..", ParentDir),
            (r".\a", CurrentDir),
            (r"a\\b", EmptyComponent),
            (r"a\", EmptyComponent),
            ("a//b", EmptyComponent),
            (r"C:\x", Colon),
            ("a:stream", Colon),
            ("a\0b", ControlChar),
            ("a\nb", ControlChar),
            ("a\r", ControlChar),
            (long.as_str(), TooLong),
        ] {
            assert_eq!(RelPath::parse(raw), Err(why), "{raw:?}");
        }
    }

    #[test]
    fn checksum_minus_one_is_absent_and_values_are_signed_decimal() {
        assert_eq!(Checksum::parse("-1"), Ok(Checksum::Absent));
        assert_eq!(Checksum::parse("351459945"), Ok(Checksum::Crc(Crc32(351459945))));
        assert_eq!(Checksum::parse("-1793887741"), Ok(Checksum::Crc(Crc32(2501079555))));
        assert_eq!(Checksum::parse(" 7\r\n"), Ok(Checksum::Crc(Crc32(7))));
        assert_eq!(Crc32(2501079555).to_string(), "-1793887741");
        for bad in ["", "abc", "4294967295", "1.5", "0x10"] {
            assert!(Checksum::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn version_is_exactly_three_unsigned_parts() {
        assert_eq!(Version::parse("1.0.167050"), Ok(Version { major: 1, minor: 0, build: 167050 }));
        for bad in ["1.0", "1.0.0.0", "+1.0.0", " 1.0.0", "1..0", "1.0.x", ""] {
            assert!(Version::parse(bad).is_err(), "{bad:?}");
        }
        assert!(Version::parse("1.0.10").unwrap() > Version::parse("1.0.9").unwrap());
    }

    #[test]
    fn manifest_name_is_one_safe_segment_ending_in_manifest_xml() {
        assert!(ManifestName::parse("Zwift_1.0.167050_ce7420cb_manifest.xml").is_ok());
        for bad in ["Zwift_ver_cur.xml", "../x_manifest.xml", "a/b_manifest.xml", "foo.xml", "x_manifest.xml "] {
            assert!(ManifestName::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn server_folder_is_one_safe_segment() {
        assert!(ServerFolder::parse("Zwift_1.0.167050_ce7420cb").is_ok());
        for bad in ["", ".", "..", "a/b", r"a\b", "a b", "a%2F"] {
            assert!(ServerFolder::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn manifest_rejects_duplicates_case_collisions_file_dir_conflicts_and_reserved_paths() {
        use ManifestRejection::*;
        for (paths, why) in [
            (&[r"a\b", "a/b"][..], Duplicate(rel("a/b"))),
            (&[r"Data\x", r"data\X"][..], CaseCollision(rel("Data/x"), rel("data/X"))),
            (&["a", r"a\b"][..], FileIsAlsoDir(rel("a"))),
            (&[r"x\a", r"x\A\b"][..], FileIsAlsoDir(rel("x/a"))),
            (&["Zwift_ver_cur.xml"][..], Reserved(rel("Zwift_ver_cur.xml"))),
            (&["zwift_VER_cur.XML"][..], Reserved(rel("zwift_VER_cur.XML"))),
            (&[r".zwift-bootstrap\lock"][..], Reserved(rel(".zwift-bootstrap/lock"))),
            (&["Zwift_1.0.1_x_manifest.xml"][..], Reserved(rel("Zwift_1.0.1_x_manifest.xml"))),
        ] {
            assert_eq!(manifest(paths).map(|_| ()), Err(why), "{paths:?}");
        }
    }

    #[test]
    fn manifest_accepts_control_file_names_below_the_root() {
        let m = manifest(&[r"data\Zwift_ver_cur.xml", r"data\x_manifest.xml", "b", "a"]).unwrap();
        let paths: Vec<&str> = m.files().keys().map(RelPath::as_str).collect();
        assert_eq!(paths, ["a", "b", "data/Zwift_ver_cur.xml", "data/x_manifest.xml"]);
    }
}
