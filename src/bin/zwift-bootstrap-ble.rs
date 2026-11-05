//! This is a separate binary because the packages give it the file capability
//! cap_net_admin+ep. A binary with a file capability runs in secure mode, and glibc then
//! removes LD_PRELOAD, LD_LIBRARY_PATH and TMPDIR from the environment. `launch` must pass
//! these variables on to Wine.

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
