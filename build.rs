//! Builds the shims in `shim/` into `$OUT_DIR` with MinGW: `libusb0.dll` for ANT+ sticks,
//! `blewin10lib.dll` for Bluetooth LE, and `zwiftlauncher.exe`, the parent process of the
//! game. The binary embeds them, and `launch` writes them into the install dir.

use std::env;
use std::path::PathBuf;
use std::process::Command;

const DLL: &[&str] = &["-shared", "-Wl,--kill-at"];
const EXE: &[&str] = &["-mwindows"];

/// (source, output, kind flags, extra libraries)
const SHIMS: &[(&str, &str, &[&str], &[&str])] = &[
    ("shim/libusb0.c", "libusb0.dll", DLL, &[]),
    ("shim/blewin10lib.c", "blewin10lib.dll", DLL, &["-lws2_32"]),
    ("shim/zwiftlauncher.c", "zwiftlauncher.exe", EXE, &[]),
];

fn main() {
    println!("cargo:rerun-if-env-changed=MINGW_CC");
    let cc = env::var("MINGW_CC").unwrap_or_else(|_| "x86_64-w64-mingw32-gcc".to_owned());
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    for (source, output, kind, libs) in SHIMS {
        println!("cargo:rerun-if-changed={source}");
        let status = Command::new(&cc)
            // _WIN32_WINNT: CancelIoEx and GetModuleHandleEx need Windows 7 headers on old MinGW.
            // --no-insert-timestamp and -s: the same source gives the same DLL bytes.
            .args(["-O2", "-Wall", "-Wextra", "-D_WIN32_WINNT=0x0601", "-static-libgcc", "-s"])
            .args(*kind)
            .args(["-Wl,--no-insert-timestamp", "-o"])
            .arg(out_dir.join(output))
            .arg(source)
            .args(*libs)
            .status();
        match status {
            Ok(status) if status.success() => {}
            Ok(status) => panic!("{cc} failed to build {source}: {status}"),
            Err(error) => panic!(
                "cannot run {cc} ({error}). The shims need a MinGW-w64 C compiler: \
                 mingw-w64-gcc on Arch, gcc-mingw-w64-x86-64 on Debian and Ubuntu. \
                 MINGW_CC selects another compiler."
            ),
        }
    }
}
