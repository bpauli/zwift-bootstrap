//! Behavior tests of the library against a fake CDN on 127.0.0.1. No network, no real
//! Zwift data. Tests assert on observable results only: the bytes in the install dir,
//! the ver_cur bytes, the returned outcome or error, and which URLs were requested.
//!
//! "Crash at every point" is covered without hooks into the code: each crash state in
//! the sync.rs table is *arranged* on disk directly, then one sync must converge with
//! the expected requests. tests/cli.rs kills the real binary mid-download.

mod common;

use std::fs;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use common::*;
use zwift_bootstrap::{Error, Event, ProblemKind, Status, SyncMode, SyncOutcome};

#[test]
fn fresh_install_writes_every_file_and_the_server_ver_cur() {
    let cdn = FakeCdn::start(A);
    let dirs = dirs();
    let outcome = sync(&cdn, &dirs, SyncMode::Install);
    let SyncOutcome::Updated { from: None, to, report } = outcome else { panic!("{outcome:?}") };
    assert_eq!(to.to_string(), "1.0.1");
    assert_eq!((report.downloaded_files, report.reused_files, report.deleted_files), (5, 0, 0));
    assert_installed(&dirs, &cdn.published());
    let mtime = fs::metadata(dirs.install.join("data/Worlds/world9/map 04.ztx")).unwrap().modified().unwrap();
    assert_eq!(mtime, SystemTime::UNIX_EPOCH + Duration::from_millis(MODIFIED_UNIX_MILLIS));
}

#[test]
fn second_sync_requests_only_the_ver_cur() {
    let cdn = FakeCdn::start(A);
    let dirs = dirs();
    sync(&cdn, &dirs, SyncMode::Install);
    cdn.take_requests();
    let outcome = sync(&cdn, &dirs, SyncMode::Update);
    assert!(matches!(outcome, SyncOutcome::Current { .. }), "{outcome:?}");
    assert_eq!(cdn.take_requests(), [format!("{ROOT}Zwift_ver_cur.xml")]);
    assert_installed(&dirs, &cdn.published());
}

#[test]
fn update_fetches_changed_and_new_files_and_deletes_removed_ones() {
    let cdn = FakeCdn::start(A);
    let dirs = dirs();
    sync(&cdn, &dirs, SyncMode::Install);
    cdn.publish(B);
    cdn.take_requests();
    let outcome = sync(&cdn, &dirs, SyncMode::Update);
    let SyncOutcome::Updated { from: Some(from), report, .. } = outcome else { panic!("{outcome:?}") };
    assert_eq!(from.to_string(), "1.0.1");
    assert_eq!(cdn.game_file_requests(), ["ZwiftApp.exe", "assets/x", "data/new.bin"]);
    assert_eq!((report.downloaded_files, report.deleted_files), (3, 2));
    assert_installed(&dirs, &cdn.published());
    assert!(!dirs.install.join("data/old").exists(), "empty dirs are pruned");
}

#[test]
fn corrupt_body_is_retried_then_accepted() {
    let cdn = FakeCdn::start(A);
    cdn.fault("assets/x/y.bin", Fault::CorruptTimes(1));
    let dirs = dirs();
    let (result, events) = run(&cdn, &dirs, SyncMode::Install);
    result.unwrap();
    let requests = cdn.game_file_requests();
    assert_eq!(requests.iter().filter(|p| *p == "assets/x/y.bin").count(), 2);
    assert!(events.iter().any(|e| matches!(e, Event::Retry { path, attempt: 1, .. } if path.as_str() == "assets/x/y.bin")));
    assert_installed(&dirs, &cdn.published());
}

#[test]
fn file_that_never_verifies_fails_the_run_and_leaves_the_install_untouched() {
    let cdn = FakeCdn::start(A);
    let dirs = dirs();
    sync(&cdn, &dirs, SyncMode::Install);
    let before = tree(&dirs.install);
    cdn.publish(B);
    cdn.fault("data/new.bin", Fault::CorruptTimes(u32::MAX));
    let (result, _) = run(&cdn, &dirs, SyncMode::Update);
    let Err(Error::Files(failed)) = result else { panic!("{result:?}") };
    let failed: Vec<&str> = failed.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(failed, ["data/new.bin"]);
    assert_eq!(tree(&dirs.install), before);
}

#[test]
fn rerun_reuses_verified_staged_files() {
    let cdn = FakeCdn::start(A);
    cdn.fault("ZwiftApp.exe", Fault::Status(404));
    let dirs = dirs();
    let (result, _) = run(&cdn, &dirs, SyncMode::Install);
    assert!(matches!(result, Err(Error::Files(ref f)) if f.len() == 1), "{result:?}");
    assert!(!dirs.install.join("Zwift_ver_cur.xml").exists());
    cdn.clear_faults();
    cdn.take_requests();
    let outcome = sync(&cdn, &dirs, SyncMode::Install);
    let SyncOutcome::Updated { report, .. } = outcome else { panic!("{outcome:?}") };
    assert_eq!((report.downloaded_files, report.reused_files), (1, 4));
    let published = cdn.published();
    let folder = published.manifest_name.trim_end_matches("_manifest.xml");
    assert_eq!(
        cdn.take_requests(),
        [format!("{ROOT}Zwift_ver_cur.xml"), format!("{ROOT}{folder}/ZwiftApp.exe")],
        "the staged manifest and files are reused"
    );
    assert_installed(&dirs, &published);
}

#[test]
fn staged_file_with_wrong_content_is_downloaded_again() {
    let cdn = FakeCdn::start(A);
    let dirs = dirs();
    write(&dirs.staging.join("files/ZwiftApp.exe"), b"app v9");
    write(&dirs.staging.join("files/assets/x/y.bin"), b"asse");
    write(&dirs.staging.join("files/unchanged.dll"), b"same in every version");
    sync(&cdn, &dirs, SyncMode::Install);
    assert_eq!(
        cdn.game_file_requests(),
        ["ZwiftApp.exe", "assets/x/y.bin", "data/Worlds/world9/map 04.ztx", "data/old/gone.bin"]
    );
    assert_installed(&dirs, &cdn.published());
}

/// Lays out what a commit toward the CDN's current release leaves when it dies after
/// the marker: some targets placed, the rest still staged, some deletes done.
fn arrange_crash_mid_place(dirs: &Dirs, toward: &Published, placed: &[&str], staged: &[&str], deleted: &[&str]) {
    let content = |path: &str| toward.files.iter().find(|(p, _)| p == path).unwrap().1.clone();
    write(&dirs.install.join(&toward.manifest_name), &toward.manifest);
    write(&dirs.install.join(".zwift-bootstrap/pending"), toward.manifest_name.as_bytes());
    for path in deleted {
        fs::remove_file(dirs.install.join(path)).unwrap();
    }
    for path in placed {
        write(&dirs.install.join(path), &content(path));
    }
    for path in staged {
        write(&dirs.staging.join("files").join(path), &content(path));
    }
}

#[test]
fn crash_mid_place_converges_without_downloads() {
    let cdn = FakeCdn::start(A);
    let dirs = dirs();
    sync(&cdn, &dirs, SyncMode::Install);
    cdn.publish(B);
    let b = cdn.published();
    arrange_crash_mid_place(&dirs, &b, &["ZwiftApp.exe"], &["data/new.bin"], &["data/old/gone.bin", "assets/x/y.bin"]);
    fs::remove_dir(dirs.install.join("assets/x")).unwrap();
    write(&dirs.staging.join("files/assets/x"), b"x is a file now");
    cdn.take_requests();
    let outcome = sync(&cdn, &dirs, SyncMode::Update);
    assert!(matches!(outcome, SyncOutcome::Updated { report, .. } if report.downloaded_files == 0), "{outcome:?}");
    assert_eq!(cdn.game_file_requests(), Vec::<String>::new());
    assert_installed(&dirs, &b);
}

#[test]
fn crash_mid_place_then_new_server_version_converges() {
    let cdn = FakeCdn::start(A);
    let dirs = dirs();
    sync(&cdn, &dirs, SyncMode::Install);
    cdn.publish(B);
    let b = cdn.published();
    arrange_crash_mid_place(&dirs, &b, &["ZwiftApp.exe", "data/new.bin"], &[], &["data/old/gone.bin"]);
    // C equals A for ZwiftApp.exe and data/old/gone.bin, which the dead commit changed.
    cdn.publish(Release {
        version: "1.0.3",
        files: &[
            ("ZwiftApp.exe", b"app v1"),
            (r"data\Worlds\world9\map 04.ztx", b"map four"),
            (r"assets\x\y.bin", b"assets y"),
            (r"data\old\gone.bin", b"leaves in B"),
            ("unchanged.dll", b"same in every version"),
            ("c.bin", b"new in C"),
        ],
    });
    cdn.take_requests();
    sync(&cdn, &dirs, SyncMode::Update);
    assert_eq!(cdn.game_file_requests(), ["ZwiftApp.exe", "c.bin", "data/old/gone.bin"]);
    assert_installed(&dirs, &cdn.published());
}

#[test]
fn crash_mid_place_with_server_still_at_old_version_restores_it() {
    let cdn = FakeCdn::start(A);
    let dirs = dirs();
    sync(&cdn, &dirs, SyncMode::Install);
    let a = cdn.published();
    cdn.publish(B);
    let b = cdn.published();
    cdn.publish(A);
    arrange_crash_mid_place(&dirs, &b, &["ZwiftApp.exe", "data/new.bin"], &[], &["data/old/gone.bin"]);
    cdn.take_requests();
    let outcome = sync(&cdn, &dirs, SyncMode::Update);
    assert!(matches!(outcome, SyncOutcome::Updated { .. }), "equal versions must not hide a mixed disk: {outcome:?}");
    assert_eq!(cdn.game_file_requests(), ["ZwiftApp.exe", "data/old/gone.bin"]);
    assert_installed(&dirs, &a);
}

#[test]
fn crash_after_new_manifest_written_converges() {
    let cdn = FakeCdn::start(A);
    let dirs = dirs();
    sync(&cdn, &dirs, SyncMode::Install);
    cdn.publish(B);
    let b = cdn.published();
    write(&dirs.install.join(&b.manifest_name), &b.manifest);
    cdn.take_requests();
    sync(&cdn, &dirs, SyncMode::Update);
    assert_eq!(cdn.game_file_requests(), ["ZwiftApp.exe", "assets/x", "data/new.bin"]);
    assert_installed(&dirs, &b);
}

#[test]
fn crash_after_commit_point_finishes_cleanup_on_the_next_sync() {
    let cdn = FakeCdn::start(A);
    let dirs = dirs();
    sync(&cdn, &dirs, SyncMode::Install);
    let a = cdn.published();
    cdn.publish(B);
    sync(&cdn, &dirs, SyncMode::Update);
    let b = cdn.published();
    write(&dirs.install.join(&a.manifest_name), &a.manifest);
    write(&dirs.install.join(".zwift-bootstrap/pending"), b.manifest_name.as_bytes());
    cdn.take_requests();
    let outcome = sync(&cdn, &dirs, SyncMode::Update);
    assert!(matches!(outcome, SyncOutcome::Current { .. }), "{outcome:?}");
    assert_eq!(cdn.take_requests(), [format!("{ROOT}Zwift_ver_cur.xml")]);
    assert_installed(&dirs, &b);
}

#[test]
fn hostile_manifest_is_rejected_before_any_install_write() {
    let hostile: &[&[&str]] = &[
        &[r"..\x"],
        &[r"a\..\b"],
        &[r"\abs"],
        &[r"C:\x"],
        &[r"a\\b"],
        &["a/../b"],
        &["Zwift_ver_cur.xml"],
        &[r".zwift-bootstrap\lock"],
        &["A", "a"],
        &["a", r"a\b"],
    ];
    for paths in hostile {
        let files: Vec<(&str, &[u8])> = paths.iter().map(|p| (*p, &b"evil"[..])).collect();
        let cdn = FakeCdn::start(Release { version: "1.0.1", files: &files });
        let dirs = dirs();
        let (result, _) = run(&cdn, &dirs, SyncMode::Install);
        assert!(matches!(result, Err(Error::BadServerData { what: "manifest", .. })), "{paths:?}: {result:?}");
        assert_eq!(tree(&dirs.install).into_keys().collect::<Vec<_>>(), [".zwift-bootstrap/lock"], "{paths:?}");
        let mut top: Vec<String> = fs::read_dir(dirs.root.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        top.sort();
        assert_eq!(top, ["install", "staging"], "{paths:?}");
        assert_eq!(cdn.game_file_requests(), Vec::<String>::new(), "{paths:?}");
    }
}

#[test]
fn manifest_checksum_mismatch_is_an_error() {
    let cdn = FakeCdn::start(A);
    cdn.fault("_manifest.xml", Fault::CorruptTimes(u32::MAX));
    let dirs = dirs();
    let (result, _) = run(&cdn, &dirs, SyncMode::Install);
    assert!(matches!(result, Err(Error::ManifestChecksum { .. })), "{result:?}");
    assert_eq!(cdn.game_file_requests(), Vec::<String>::new());
    assert!(!dirs.install.join("Zwift_ver_cur.xml").exists());
}

#[test]
fn gzip_manifest_is_decoded() {
    let cdn = FakeCdn::start(A);
    cdn.fault("_manifest.xml", Fault::Gzip);
    cdn.fault("Zwift_ver_cur.xml", Fault::Gzip);
    cdn.fault("ZwiftApp.exe", Fault::Gzip);
    let dirs = dirs();
    sync(&cdn, &dirs, SyncMode::Install);
    assert_installed(&dirs, &cdn.published());
}

#[test]
fn overlong_body_is_cut_off_and_retried() {
    let cdn = FakeCdn::start(A);
    cdn.fault("unchanged.dll", Fault::TooLong(1 << 20));
    let dirs = dirs();
    sync(&cdn, &dirs, SyncMode::Install);
    let requests = cdn.game_file_requests();
    assert_eq!(requests.iter().filter(|p| *p == "unchanged.dll").count(), 2);
    assert_installed(&dirs, &cdn.published());
}

#[test]
fn update_requires_install_and_install_seeds() {
    let cdn = FakeCdn::start(A);
    let dirs = dirs();
    let (result, _) = run(&cdn, &dirs, SyncMode::Update);
    assert!(matches!(result, Err(Error::NotInstalled { .. })), "{result:?}");
    assert!(!dirs.install.join("Zwift_ver_cur.xml").exists());
    let outcome = sync(&cdn, &dirs, SyncMode::Install);
    assert!(matches!(outcome, SyncOutcome::Updated { from: None, .. }), "{outcome:?}");
    assert_installed(&dirs, &cdn.published());
}

#[test]
fn update_accepts_the_windows_launcher_seed() {
    let cdn = FakeCdn::start(A);
    let dirs = dirs();
    write(
        &dirs.install.join("Zwift_ver_cur.xml"),
        br#"<Zwift version="0.0.0" GAME_URL="https://us-or-rly101.zwift.com" manifest="Zwift_0.0.0_manifest.xml"/>"#,
    );
    write(&dirs.install.join("Zwift_0.0.0_manifest.xml"), br#"<Zwift version="0.0.0" folder="Zwift_0.0.0"><files></files></Zwift>"#);
    let outcome = sync(&cdn, &dirs, SyncMode::Update);
    let SyncOutcome::Updated { from: Some(from), .. } = outcome else { panic!("{outcome:?}") };
    assert_eq!(from.to_string(), "0.0.0");
    assert_installed(&dirs, &cdn.published());
}

#[test]
fn verify_reports_missing_wrong_length_and_wrong_crc() {
    let cdn = FakeCdn::start(A);
    let dirs = dirs();
    sync(&cdn, &dirs, SyncMode::Install);
    let bs = bootstrapper(&cdn, &dirs);
    let clean = bs.verify(&mut |_| {}).unwrap();
    assert_eq!((clean.checked, clean.problems.len()), (5, 0));

    fs::remove_file(dirs.install.join("ZwiftApp.exe")).unwrap();
    fs::write(dirs.install.join("unchanged.dll"), b"short").unwrap();
    fs::write(dirs.install.join("assets/x/y.bin"), b"assets Y").unwrap();
    cdn.take_requests();
    let report = bs.verify(&mut |_| {}).unwrap();
    let mut problems: Vec<String> = report
        .problems
        .iter()
        .map(|p| match &p.kind {
            ProblemKind::Missing => format!("{} missing", p.path),
            ProblemKind::WrongLength { expected, actual } => format!("{} length {actual} not {expected}", p.path),
            ProblemKind::WrongCrc { .. } => format!("{} crc", p.path),
        })
        .collect();
    problems.sort();
    assert_eq!(problems, ["ZwiftApp.exe missing", "assets/x/y.bin crc", "unchanged.dll length 5 not 21"]);
    assert_eq!(cdn.take_requests(), Vec::<String>::new(), "verify never touches the network");
}

#[test]
fn check_reports_the_plan_and_writes_nothing_in_the_install() {
    let cdn = FakeCdn::start(A);
    let dirs = dirs();
    let status = bootstrapper(&cdn, &dirs).check(&mut |_| {}).unwrap();
    let Status::UpdateAvailable { local: None, server, plan, .. } = status else { panic!("{status:?}") };
    assert_eq!(server.to_string(), "1.0.1");
    assert_eq!((plan.fetch_files, plan.fetch_bytes, plan.delete_files), (5, 6 + 8 + 8 + 11 + 21, 0));
    assert!(!dirs.install.exists());

    sync(&cdn, &dirs, SyncMode::Install);
    cdn.publish(B);
    let before = tree(&dirs.install);
    let status = bootstrapper(&cdn, &dirs).check(&mut |_| {}).unwrap();
    let Status::UpdateAvailable { local: Some(local), plan, .. } = status else { panic!("{status:?}") };
    assert_eq!(local.to_string(), "1.0.1");
    assert_eq!((plan.fetch_files, plan.fetch_bytes, plan.delete_files), (3, 7 + 15 + 8, 2));
    assert_eq!(tree(&dirs.install), before);
}

#[test]
fn second_concurrent_sync_is_busy() {
    let slow = vec![1u8; 32 * 1024];
    let cdn = FakeCdn::start(Release { version: "1.0.1", files: &[("slow.bin", &slow)] });
    cdn.fault("slow.bin", Fault::Slow);
    let dirs = dirs();
    thread::scope(|s| {
        let first = s.spawn(|| run(&cdn, &dirs, SyncMode::Install).0);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !cdn.requests.lock().unwrap().iter().any(|r| r.ends_with("slow.bin")) {
            assert!(Instant::now() < deadline, "first sync never started downloading");
            thread::sleep(Duration::from_millis(5));
        }
        let (second, _) = run(&cdn, &dirs, SyncMode::Install);
        assert!(matches!(second, Err(Error::Busy(_))), "{second:?}");
        first.join().unwrap().unwrap();
    });
    assert_installed(&dirs, &cdn.published());
}
