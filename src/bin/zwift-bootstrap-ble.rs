//! The Bluetooth LE bridge that `launch` starts. Its own binary, because the packages give
//! it the file capability cap_net_admin+ep for the management socket of the kernel, and a
//! binary with a file capability runs in secure mode: glibc then strips LD_PRELOAD,
//! LD_LIBRARY_PATH and TMPDIR from the environment, which `launch` must pass on to Wine.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

#[derive(Parser)]
#[command(name = "zwift-bootstrap-ble", version)]
struct Cli {
    socket: PathBuf,
    /// The bridge ends when this process is no longer its parent.
    #[arg(long)]
    parent: u32,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match zwift_bootstrap::ble::serve(&cli.socket, cli.parent) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("zwift-bootstrap-ble: {e}");
            ExitCode::from(2)
        }
    }
}
