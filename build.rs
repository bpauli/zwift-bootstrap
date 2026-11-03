//! Builds the ANT+ shim `shim/libusb0.c` into `$OUT_DIR/libusb0.dll` with MinGW.
//! The binary embeds the DLL, and `launch` writes it next to ZwiftApp.exe.

use std::env;
use std::path::PathBuf;
use std::process::Command;

const SOURCE: &str = "shim/libusb0.c";

fn main() {
    println!("cargo:rerun-if-changed={SOURCE}");
    println!("cargo:rerun-if-env-changed=MINGW_CC");
    let cc = env::var("MINGW_CC").unwrap_or_else(|_| "x86_64-w64-mingw32-gcc".to_owned());
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR")).join("libusb0.dll");
    let status = Command::new(&cc)
        // _WIN32_WINNT: CancelIoEx and GetModuleHandleEx need Windows 7 headers on old MinGW.
        // --no-insert-timestamp and -s: the same source gives the same DLL bytes.
        .args(["-O2", "-Wall", "-Wextra", "-D_WIN32_WINNT=0x0601", "-shared", "-static-libgcc", "-s"])
        .args(["-Wl,--kill-at", "-Wl,--no-insert-timestamp", "-o"])
        .arg(&out)
        .arg(SOURCE)
        .status();
    match status {
        Ok(status) if status.success() => {}
        Ok(status) => panic!("{cc} failed to build {SOURCE}: {status}"),
        Err(error) => panic!(
            "cannot run {cc} ({error}). The ANT+ shim needs a MinGW-w64 C compiler: \
             mingw-w64-gcc on Arch, gcc-mingw-w64-x86-64 on Debian and Ubuntu. \
             MINGW_CC selects another compiler."
        ),
    }
}
