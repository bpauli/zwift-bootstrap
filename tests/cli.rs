//! Tests of the `zwift-bootstrap` binary against the fake CDN.
//!
//! A separate test binary from tests/sync.rs on purpose: a fork copies every fd of the
//! test process until exec, including the flock fds of syncs running in other test
//! threads, so spawning the binary next to in-process syncs makes their locks flaky.

mod common;

use std::fs;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use common::*;

/// The binary with its staging dir under `dirs.staging` (XDG_CACHE_HOME/zwift/downloads).
/// `args[0]` is the subcommand; `--dir` goes right after it.
fn binary(dirs: &Dirs, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zwift-bootstrap"));
    command
        .arg(args[0])
        .arg("--dir")
        .arg(&dirs.install)
        .args(&args[1..])
        .env("XDG_CACHE_HOME", dirs.root.path().join("cache"))
        .stdin(Stdio::null());
    command
}

fn binary_dirs() -> Dirs {
    let dirs = dirs();
    let staging = dirs.root.path().join("cache/zwift/downloads");
    Dirs { staging, ..dirs }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn killed_binary_resumes_on_rerun() {
    let slow = vec![7u8; 256 * 1024];
    let files: &[(&str, &[u8])] = &[("a_slow.bin", &slow), ("b.bin", b"bee"), (r"c\c.bin", b"sea")];
    let cdn = FakeCdn::start(Release { version: "1.0.1", files });
    cdn.fault("a_slow.bin", Fault::Slow);
    let dirs = binary_dirs();
    let mut child = binary(&dirs, &["install", "--jobs", "2", "--base-url", &cdn.base_url])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let staged = dirs.staging.join("files");
    let deadline = Instant::now() + Duration::from_secs(20);
    let slow_started = || cdn.requests.lock().unwrap().iter().any(|r| r.ends_with("a_slow.bin"));
    while !(staged.join("b.bin").exists() && staged.join("c/c.bin").exists() && slow_started()) {
        assert!(Instant::now() < deadline, "the first run never staged the fast files");
        thread::sleep(Duration::from_millis(5));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(!staged.join("a_slow.bin").exists(), "killed before the slow file finished");
    assert!(!dirs.install.join("Zwift_ver_cur.xml").exists());

    cdn.clear_faults();
    cdn.take_requests();
    let output = binary(&dirs, &["install", "--base-url", &cdn.base_url]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(stdout(&output).starts_with("installed 1.0.1: 1 downloaded"), "{}", stdout(&output));
    let published = cdn.published();
    let folder = published.manifest_name.trim_end_matches("_manifest.xml");
    assert_eq!(cdn.take_requests(), [format!("{ROOT}Zwift_ver_cur.xml"), format!("{ROOT}{folder}/a_slow.bin")]);
    assert_installed(&dirs, &published);
}

#[test]
fn check_command_prints_versions_and_plan() {
    let cdn = FakeCdn::start(A);
    let dirs = binary_dirs();
    let output = binary(&dirs, &["check", "--base-url", &cdn.base_url]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(stdout(&output), "local none, server 1.0.1 (update available)\nfetch 5 files (54 B), delete 0 files\n");

    binary(&dirs, &["install", "--base-url", &cdn.base_url]).output().unwrap();
    let output = binary(&dirs, &["check", "--base-url", &cdn.base_url]).output().unwrap();
    assert_eq!(stdout(&output), "local 1.0.1, server 1.0.1 (up to date)\n");
}

#[test]
fn verify_command_exits_non_zero_on_problems() {
    let cdn = FakeCdn::start(A);
    let dirs = binary_dirs();
    let ok = binary(&dirs, &["verify"]).output().unwrap();
    assert_eq!(ok.status.code(), Some(2), "no install is an error");

    binary(&dirs, &["install", "--base-url", &cdn.base_url]).output().unwrap();
    let ok = binary(&dirs, &["verify"]).output().unwrap();
    assert_eq!((ok.status.code(), stdout(&ok)), (Some(0), "5 files checked, 0 problems\n".to_owned()));

    fs::remove_file(dirs.install.join("unchanged.dll")).unwrap();
    let bad = binary(&dirs, &["verify"]).output().unwrap();
    assert_eq!(bad.status.code(), Some(1));
    assert_eq!(stdout(&bad), "unchanged.dll: missing\n5 files checked, 1 problems\n");
}

#[test]
fn launch_runs_wine_with_launcher_args_in_the_install_dir() {
    let script: &[u8] = b"printf '%s\\n' \"$PWD\" \"$WINEPREFIX\" \"$@\"\n";
    let cdn = FakeCdn::start(Release { version: "1.0.1", files: &[("ZwiftApp.exe", script)] });
    let dirs = binary_dirs();
    binary(&dirs, &["install", "--base-url", &cdn.base_url]).output().unwrap();
    let data_home = dirs.root.path().join("data");
    let output = binary(&dirs, &["launch", "--", "--extra", "two words"])
        .env("WINE", "sh")
        .env("XDG_DATA_HOME", &data_home)
        .env_remove("WINEPREFIX")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let install = fs::canonicalize(&dirs.install).unwrap();
    assert_eq!(
        stdout(&output),
        format!(
            "{}\n{}\n--launcher_version=1.2.0\n--token=\n--extra\ntwo words\n",
            install.display(),
            data_home.join("zwift-wine").display()
        )
    );
}

#[test]
fn launch_keeps_a_wineprefix_from_the_environment() {
    let script: &[u8] = b"printf '%s\\n' \"$WINEPREFIX\"\n";
    let cdn = FakeCdn::start(Release { version: "1.0.1", files: &[("ZwiftApp.exe", script)] });
    let dirs = binary_dirs();
    binary(&dirs, &["install", "--base-url", &cdn.base_url]).output().unwrap();
    let output =
        binary(&dirs, &["launch"]).env("WINE", "sh").env("WINEPREFIX", "/opt/my-prefix").output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(stdout(&output), "/opt/my-prefix\n");
}
