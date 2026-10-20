//! Pure decisions. No I/O. Everything here is unit-tested with in-memory values.

use crate::PlanSummary;
use crate::model::{Checksum, FileEntry, Manifest, RelPath, VerCur, Version};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Assessment {
    Current,
    Update { mandatory: bool },
}

/// Protocol step 3. Any difference is an update, a lower server version included.
/// `mandatory` when the server names a `mandatory_version` greater than `local`.
///
/// `interrupted`: a commit died before its commit point, so the disk may mix two
/// versions even when the local ver_cur equals the server's (the server rolled back, or
/// never moved). Equal versions do not prove a clean disk then; only a diff does.
///
/// Not ported: the `Zwift_ver_cur_filename.txt` rule. This tool always uses the fixed
/// name `Zwift_ver_cur.xml`, so that rule can never fire.
pub(crate) fn assess(local: Version, interrupted: bool, server: &VerCur) -> Assessment {
    if local == server.version && !interrupted {
        return Assessment::Current;
    }
    Assessment::Update { mandatory: server.mandatory_version.is_some_and(|m| m > local) }
}

/// One file the install must end up with, and the content it must have.
#[derive(Clone, Debug)]
pub(crate) struct Target {
    pub path: RelPath,
    pub entry: FileEntry,
}

/// The difference between two manifests. Says *what* must change, not *how*: whether a
/// target needs a download is decided per file at stage time by content (see sync.rs),
/// which is what makes a rerun after any crash converge.
pub(crate) struct Plan {
    /// Server entries whose content the local manifest does not vouch for. Path order.
    pub fetch: Vec<Target>,
    /// Local entries the server manifest no longer has.
    pub delete: Vec<RelPath>,
}

/// Protocol step 4, extended for an interrupted commit.
///
/// `local` is the manifest the local ver_cur names. `pending` is the manifest an
/// interrupted commit was moving toward (see `disk::LocalState`), if any. A path is
/// unchanged only when every manifest that may describe its bytes on disk agrees with
/// the server:
///
///   unchanged(p) = same(local[p], server[p])
///                  && pending.map_or(true, |b| b.get(p).is_some_and(|e| same(e, server[p])))
///
/// `delete` = (local paths + pending paths) - server paths.
///
/// Without `pending` this is a plain checksum diff. With it, a crash mid-commit followed by
/// a new server version still converges: files the dead commit replaced or deleted are
/// not vouched for by the old manifest any more, so they go to `fetch`, where content
/// decides. When `pending` is the server manifest (the usual rerun), the extra term is
/// a no-op and the plan is as small as the plain diff.
///
/// An `Absent` checksum is never proof of sameness, so such a file is always fetched.
pub(crate) fn diff(local: &Manifest, pending: Option<&Manifest>, server: &Manifest) -> Plan {
    let vouched = |m: &Manifest, path: &RelPath, want: &FileEntry| {
        m.files().get(path).is_some_and(|have| same(have, want))
    };
    let fetch = server
        .files()
        .iter()
        .filter(|(path, entry)| {
            !(vouched(local, path, entry) && pending.is_none_or(|p| vouched(p, path, entry)))
        })
        .map(|(path, entry)| Target { path: path.clone(), entry: entry.clone() })
        .collect();
    let mut delete: Vec<RelPath> = local
        .files()
        .keys()
        .chain(pending.into_iter().flat_map(|p| p.files().keys()))
        .filter(|path| !server.files().contains_key(*path))
        .cloned()
        .collect();
    delete.sort();
    delete.dedup();
    Plan { fetch, delete }
}

fn same(local: &FileEntry, server: &FileEntry) -> bool {
    matches!((local.checksum, server.checksum), (Checksum::Crc(a), Checksum::Crc(b)) if a == b)
        && local.length == server.length
}

impl Plan {
    pub(crate) fn summary(&self) -> PlanSummary {
        PlanSummary {
            fetch_files: self.fetch.len(),
            fetch_bytes: self.fetch.iter().map(|t| t.entry.length).sum(),
            delete_files: self.delete.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Crc32, ManifestName, ServerFolder};

    /// `(path, length, checksum)`; checksum -1 is Absent.
    fn manifest(files: &[(&str, u64, i32)]) -> Manifest {
        let entries = files
            .iter()
            .map(|&(path, length, crc)| {
                let checksum = if crc == -1 { Checksum::Absent } else { Checksum::Crc(Crc32(crc as u32)) };
                (RelPath::parse(path).unwrap(), FileEntry { length, checksum, modified: None })
            })
            .collect();
        Manifest::new(Version::ZERO, ServerFolder::parse("F").unwrap(), entries).unwrap()
    }

    fn fetched(plan: &Plan) -> Vec<&str> {
        plan.fetch.iter().map(|t| t.path.as_str()).collect()
    }

    fn deleted(plan: &Plan) -> Vec<&str> {
        plan.delete.iter().map(RelPath::as_str).collect()
    }

    fn ver_cur(version: &str, mandatory: Option<&str>) -> VerCur {
        VerCur::new(
            Version::parse(version).unwrap(),
            ManifestName::parse("x_manifest.xml").unwrap(),
            Checksum::Absent,
            mandatory.map(|m| Version::parse(m).unwrap()),
            Box::new([]),
        )
    }

    #[test]
    fn assess_treats_any_version_difference_as_an_update() {
        let v = |s| Version::parse(s).unwrap();
        assert_eq!(assess(v("1.0.5"), false, &ver_cur("1.0.5", None)), Assessment::Current);
        assert_eq!(assess(v("1.0.5"), false, &ver_cur("1.0.6", None)), Assessment::Update { mandatory: false });
        assert_eq!(assess(v("1.0.5"), false, &ver_cur("1.0.4", None)), Assessment::Update { mandatory: false });
        assert_eq!(
            assess(v("1.0.5"), false, &ver_cur("1.0.9", Some("1.0.6"))),
            Assessment::Update { mandatory: true }
        );
        assert_eq!(
            assess(v("1.0.5"), false, &ver_cur("1.0.9", Some("1.0.5"))),
            Assessment::Update { mandatory: false }
        );
    }

    #[test]
    fn assess_never_calls_an_interrupted_commit_current() {
        let v = Version::parse("1.0.5").unwrap();
        assert_eq!(assess(v, true, &ver_cur("1.0.5", None)), Assessment::Update { mandatory: false });
    }

    #[test]
    fn diff_fetches_changed_and_new_and_deletes_removed() {
        let local = manifest(&[("same", 1, 10), ("changed_crc", 1, 20), ("changed_len", 1, 30), ("gone", 1, 40)]);
        let server = manifest(&[("same", 1, 10), ("changed_crc", 1, 21), ("changed_len", 2, 30), ("new", 5, 50)]);
        let plan = diff(&local, None, &server);
        assert_eq!(fetched(&plan), ["changed_crc", "changed_len", "new"]);
        assert_eq!(deleted(&plan), ["gone"]);
        let summary = plan.summary();
        assert_eq!((summary.fetch_files, summary.fetch_bytes, summary.delete_files), (3, 8, 1));
    }

    #[test]
    fn diff_against_the_empty_seed_fetches_everything() {
        let server = manifest(&[("a", 1, 1), ("b", 2, 2)]);
        let plan = diff(&Manifest::empty(), None, &server);
        assert_eq!(fetched(&plan), ["a", "b"]);
        assert!(plan.delete.is_empty());
    }

    #[test]
    fn absent_checksum_is_never_proof_of_sameness() {
        let local = manifest(&[("a", 1, -1)]);
        let plan = diff(&local, None, &manifest(&[("a", 1, -1)]));
        assert_eq!(fetched(&plan), ["a"]);
    }

    #[test]
    fn pending_equal_to_server_is_a_no_op() {
        let local = manifest(&[("a", 1, 1), ("b", 1, 2), ("old", 1, 3)]);
        let server = manifest(&[("a", 1, 1), ("b", 1, 9), ("new", 1, 4)]);
        let with = diff(&local, Some(&server), &server);
        let without = diff(&local, None, &server);
        assert_eq!(fetched(&with), fetched(&without));
        assert_eq!(deleted(&with), deleted(&without));
    }

    #[test]
    fn pending_marker_unvouches_paths_the_dead_commit_touched() {
        // Old A, dead commit toward B, server now at C.
        let a = manifest(&[("kept", 1, 1), ("b_changed", 1, 2), ("b_deleted", 1, 3), ("a_only", 1, 4)]);
        let b = manifest(&[("kept", 1, 1), ("b_changed", 1, 22), ("b_added", 1, 5)]);
        let c = manifest(&[("kept", 1, 1), ("b_changed", 1, 2), ("b_deleted", 1, 3), ("c_added", 1, 6)]);
        let plan = diff(&a, Some(&b), &c);
        assert_eq!(fetched(&plan), ["b_changed", "b_deleted", "c_added"]);
        assert_eq!(deleted(&plan), ["a_only", "b_added"]);
    }

    #[test]
    fn unknown_pending_content_checks_every_target() {
        let a = manifest(&[("x", 1, 1), ("y", 1, 2)]);
        let plan = diff(&a, Some(&Manifest::empty()), &a);
        assert_eq!(fetched(&plan), ["x", "y"]);
        assert!(plan.delete.is_empty());
    }
}
