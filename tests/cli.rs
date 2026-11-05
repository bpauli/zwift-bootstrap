//! Tests of the `zwift-bootstrap` binary against the fake CDN.
//!
//! A separate test binary from tests/sync.rs on purpose: a fork copies every fd of the
//! test process until exec, including the flock fds of syncs running in other test
//! threads, so spawning the binary next to in-process syncs makes their locks flaky.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
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
        .env("XDG_RUNTIME_DIR", dirs.root.path())
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

/// A `$WINE` that prints the program it was asked to run and the first two bytes of that
/// program, then runs the rest of its arguments with `sh`.
fn fake_wine(dirs: &Dirs) -> PathBuf {
    let path = dirs.root.path().join("wine");
    fs::write(&path, "#!/bin/sh\nprintf '%s %s\\n' \"$1\" \"$(head -c 2 \"$1\")\"\nshift\nexec sh \"$@\"\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// The first line that `fake_wine` prints when `launch` runs the game through the stub.
fn stub_line(dirs: &Dirs) -> String {
    format!("{} MZ\n", dirs.install.join(".zwift-bootstrap/ZwiftLauncher.exe").display())
}

#[test]
fn launch_runs_the_game_through_a_launcher_stub_in_the_install_dir() {
    let script: &[u8] = b"printf '%s\\n' \"$PWD\" \"$WINEPREFIX\" \"$@\"\n";
    let cdn = FakeCdn::start(Release { version: "1.0.1", files: &[("ZwiftApp.exe", script)] });
    let dirs = binary_dirs();
    binary(&dirs, &["install", "--base-url", &cdn.base_url]).output().unwrap();
    let data_home = dirs.root.path().join("data");
    let output = binary(&dirs, &["launch", "--", "--extra", "two words"])
        .env("WINE", fake_wine(&dirs))
        .env("XDG_DATA_HOME", &data_home)
        .env_remove("WINEPREFIX")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let install = fs::canonicalize(&dirs.install).unwrap();
    assert_eq!(
        stdout(&output),
        format!(
            "{}{}\n{}\n--launcher_version=1.2.0\n--extra\ntwo words\n",
            stub_line(&dirs),
            install.display(),
            data_home.join("zwift-wine").display()
        )
    );
}

#[test]
fn launch_writes_the_shims_and_verify_accepts_the_replaced_game_files() {
    let script: &[u8] = b"head -c 2 libusb0.dll; head -c 2 BleWin10Lib.dll; head -c 2 BleWin10Lib_fallback.dll; \
        printf '\\n%s\\n' \"$ZWIFT_BLE_SOCKET\"\n";
    let files: &[(&str, &[u8])] =
        &[("ZwiftApp.exe", script), ("BleWin10Lib_fallback.dll", b"original"), ("BleWin10Lib.dll", b"original")];
    let cdn = FakeCdn::start(Release { version: "1.0.1", files });
    let dirs = binary_dirs();
    binary(&dirs, &["install", "--base-url", &cdn.base_url]).output().unwrap();
    fs::write(dirs.install.join("libusb0.dll"), b"stale").unwrap();
    let output = binary(&dirs, &["launch"]).env("WINE", fake_wine(&dirs)).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let socket = dirs.root.path().join("zwift-bootstrap-ble.sock");
    assert_eq!(stdout(&output), format!("{}MZMZMZ\n{}\n", stub_line(&dirs), socket.display()));
    let contains = |file: &str, text: &str| {
        let bytes = fs::read(dirs.install.join(file)).unwrap();
        bytes.windows(text.len()).any(|w| w == text.as_bytes())
    };
    for export in ["usb_open", "usb_reap_async_nocancel", "usb_bulk_write"] {
        assert!(contains("libusb0.dll", export), "{export}");
    }
    for export in ["BLEStartScanning", "BLEPairToDevice", "BLEWriteToDevice", "BLESetProcessBLEResponse"] {
        assert!(contains("BleWin10Lib_fallback.dll", export), "{export}");
    }
    let ble = |name: &str| fs::read(dirs.install.join(name)).unwrap();
    assert_eq!(ble("BleWin10Lib.dll"), ble("BleWin10Lib_fallback.dll"));

    let output = binary(&dirs, &["verify"]).output().unwrap();
    assert_eq!(stdout(&output), "3 files checked, 0 problems\n", "a shim is not a problem");
    fs::write(dirs.install.join("BleWin10Lib.dll"), b"tampered").unwrap();
    let report = stdout(&binary(&dirs, &["verify"]).output().unwrap());
    assert!(report.starts_with("BleWin10Lib.dll: crc ") && report.ends_with("\n3 files checked, 1 problems\n"), "{report}");
}

#[test]
fn launch_keeps_a_wineprefix_from_the_environment() {
    let script: &[u8] = b"printf '%s\\n' \"$WINEPREFIX\"\n";
    let cdn = FakeCdn::start(Release { version: "1.0.1", files: &[("ZwiftApp.exe", script)] });
    let dirs = binary_dirs();
    binary(&dirs, &["install", "--base-url", &cdn.base_url]).output().unwrap();
    let output =
        binary(&dirs, &["launch"]).env("WINE", fake_wine(&dirs)).env("WINEPREFIX", "/opt/my-prefix").output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(stdout(&output), format!("{}/opt/my-prefix\n", stub_line(&dirs)));
}

#[test]
fn install_window_without_a_display_installs_in_text_mode() {
    let cdn = FakeCdn::start(A);
    let plain = binary(&binary_dirs(), &["install", "--base-url", &cdn.base_url]).output().unwrap();
    let dirs = binary_dirs();
    let output = binary(&dirs, &["install", "--window", "--base-url", &cdn.base_url])
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(stdout(&output), stdout(&plain));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.matches("warning: cannot open the progress window (").count(), 1, "{stderr}");
    assert_installed(&dirs, &cdn.published());
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn graphics_command_sets_and_shows_the_setting() {
    let dirs = binary_dirs();
    let show = || stdout(&binary(&dirs, &["graphics"]).output().unwrap());
    assert_eq!(show(), "auto\n", "no setting is auto");
    assert!(binary(&dirs, &["graphics", "ultra"]).output().unwrap().status.success());
    assert_eq!(show(), "ultra\n");
    let bad = binary(&dirs, &["graphics", "fast"]).output().unwrap();
    assert_eq!(bad.status.code(), Some(2));
    assert_eq!(show(), "ultra\n", "an unknown word changes nothing");
    assert!(binary(&dirs, &["graphics", "auto"]).output().unwrap().status.success());
    assert_eq!(show(), "auto\n");

    let file = dirs.install.join(".zwift-bootstrap/graphics");
    fs::write(&file, "fast\n").unwrap();
    let bad = binary(&dirs, &["graphics"]).output().unwrap();
    assert_eq!(bad.status.code(), Some(2));
    assert!(stderr(&bad).contains(&file.display().to_string()), "{}", stderr(&bad));
}

const PROFILE_FILES: [&str; 4] =
    ["data/configs/basic.txt", "data/configs/medium.txt", "data/configs/high.txt", "data/configs/ultra.txt"];

/// A game with the four profile files of `contents` (basic, medium, high, ultra), installed.
fn install_with_profiles(contents: [&'static [u8]; 4]) -> (FakeCdn, Dirs) {
    let files: Vec<(&str, &[u8])> = std::iter::once(("ZwiftApp.exe", &b"true\n"[..]))
        .chain(PROFILE_FILES.iter().copied().zip(contents))
        .collect();
    let cdn = FakeCdn::start(Release { version: "1.0.1", files: &files });
    let dirs = binary_dirs();
    binary(&dirs, &["install", "--base-url", &cdn.base_url]).output().unwrap();
    (cdn, dirs)
}

const ORIGINALS: [&[u8]; 4] = [
    b"res 1024x576(0x)\nset gSSAO=0\n",
    b"res 1280x720(0x)\n",
    b"set gSSAO=1\n",
    b"res 1920x1080(0x)\nsres 2048x2048\n",
];

fn profile_files(dirs: &Dirs) -> Vec<Vec<u8>> {
    PROFILE_FILES.iter().map(|file| fs::read(dirs.install.join(file)).unwrap()).collect()
}

/// Runs `launch` with the fake Wine and returns its one `graphics:` line.
fn launch_graphics_line(dirs: &Dirs) -> String {
    let output = binary(dirs, &["launch"]).env("WINE", fake_wine(dirs)).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let stderr = stderr(&output);
    let lines: Vec<&str> = stderr.lines().filter(|line| line.starts_with("graphics: ")).collect();
    assert_eq!(lines.len(), 1, "{stderr}");
    lines[0].to_owned()
}

#[test]
fn launch_writes_the_set_profile_into_all_four_profile_files_and_verify_accepts_them() {
    let (_cdn, dirs) = install_with_profiles(ORIGINALS);
    binary(&dirs, &["graphics", "high"]).output().unwrap();
    assert_eq!(launch_graphics_line(&dirs), "graphics: high (set with zwift-bootstrap graphics)");
    assert_eq!(profile_files(&dirs), [ORIGINALS[2]; 4]);

    let report = binary(&dirs, &["verify"]).output().unwrap();
    assert_eq!(stdout(&report), "5 files checked, 0 problems\n", "a swapped profile file is not a problem");
    fs::write(dirs.install.join(PROFILE_FILES[0]), b"tampered").unwrap();
    let report = stdout(&binary(&dirs, &["verify"]).output().unwrap());
    let tampered = report.starts_with("data/configs/basic.txt: length 8, ");
    assert!(tampered && report.ends_with("\n5 files checked, 1 problems\n"), "{report}");

    binary(&dirs, &["graphics", "ultra"]).output().unwrap();
    assert_eq!(launch_graphics_line(&dirs), "graphics: ultra (set with zwift-bootstrap graphics)");
    assert_eq!(profile_files(&dirs), [ORIGINALS[3]; 4], "the originals outlive the swap");
}

#[test]
fn launch_without_an_original_of_the_profile_leaves_the_choice_to_the_game() {
    let (_cdn, dirs) = install_with_profiles(ORIGINALS);
    fs::write(dirs.install.join(PROFILE_FILES[3]), b"mine").unwrap();
    binary(&dirs, &["graphics", "ultra"]).output().unwrap();
    assert_eq!(launch_graphics_line(&dirs), "graphics: game default (no original copy of data/configs/ultra.txt)");
    assert_eq!(profile_files(&dirs), [ORIGINALS[0], ORIGINALS[1], ORIGINALS[2], b"mine"]);

    binary(&dirs, &["graphics", "high"]).output().unwrap();
    launch_graphics_line(&dirs);
    assert_eq!(profile_files(&dirs), [ORIGINALS[2]; 4]);

    binary(&dirs, &["graphics", "ultra"]).output().unwrap();
    assert_eq!(launch_graphics_line(&dirs), "graphics: game default (no original copy of data/configs/ultra.txt)");
    let restored = [ORIGINALS[0], ORIGINALS[1], ORIGINALS[2], ORIGINALS[2]];
    assert_eq!(profile_files(&dirs), restored, "the game default restores the originals it has");
}

#[test]
fn launch_after_an_update_writes_the_new_original_of_the_profile() {
    let (cdn, dirs) = install_with_profiles(ORIGINALS);
    binary(&dirs, &["graphics", "ultra"]).output().unwrap();
    launch_graphics_line(&dirs);
    let new_ultra: &[u8] = b"res 2560x1440(0x)\n";
    let files: Vec<(&str, &[u8])> = std::iter::once(("ZwiftApp.exe", &b"true\n"[..]))
        .chain(PROFILE_FILES.iter().copied().zip([ORIGINALS[0], ORIGINALS[1], ORIGINALS[2], new_ultra]))
        .collect();
    cdn.publish(Release { version: "1.0.2", files: &files });
    let update = binary(&dirs, &["update", "--base-url", &cdn.base_url]).output().unwrap();
    assert!(update.status.success(), "{update:?}");
    binary(&dirs, &["graphics", "high"]).output().unwrap();
    launch_graphics_line(&dirs);
    binary(&dirs, &["graphics", "ultra"]).output().unwrap();
    launch_graphics_line(&dirs);
    assert_eq!(profile_files(&dirs), [new_ultra; 4], "the copy of ultra.txt is the new original");
}

#[test]
fn launch_with_no_profile_files_leaves_the_choice_to_the_game() {
    let cdn = FakeCdn::start(Release { version: "1.0.1", files: &[("ZwiftApp.exe", b"true\n")] });
    let dirs = binary_dirs();
    binary(&dirs, &["install", "--base-url", &cdn.base_url]).output().unwrap();
    assert_eq!(launch_graphics_line(&dirs), "graphics: game default (no original profile files)");
}
