//! The shell: argv and environment in, library calls, text out, exit code.
//! Owns the XDG defaults, the progress line and the progress window (window.rs).
//! No protocol knowledge.

mod window;

use std::collections::VecDeque;
use std::env;
use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand};
use zwift_bootstrap::{
    Bootstrapper, Config, Error, Event, GraphicsSetting, Phase, ProblemKind, Status, SyncMode, SyncOutcome, Tally,
};

/// For the commands without `--jobs`. Only `verify` runs a pool with it.
const FIXED_JOBS: NonZeroUsize = NonZeroUsize::new(8).unwrap();

#[derive(Parser)]
#[command(name = "zwift-bootstrap", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show local and server versions and what an update would download and delete.
    Check(Common),
    /// Like `update`, but an empty or invalid directory starts from version 0.0.0.
    Install(SyncArgs),
    /// Bring an existing install to the server version.
    Update(SyncArgs),
    /// Check every installed file against the local manifest. Exit 1 on any problem.
    /// Runs on 8 threads; the fixed command surface gives it no --jobs.
    Verify(Common),
    /// Run ZwiftApp.exe through $WINE (default `wine`) in $WINEPREFIX
    /// (default $XDG_DATA_HOME/zwift-wine). Writes the ANT+ and Bluetooth LE shims next to
    /// it, writes the graphics profile and starts the Bluetooth LE bridge for the game.
    Launch {
        #[command(flatten)]
        common: Common,
        #[arg(last = true)]
        extra: Vec<OsString>,
    },
    /// Show the graphics profile that `launch` writes, or set it. `auto` (the default) picks
    /// the profile from a GPU test before each start.
    Graphics {
        #[command(flatten)]
        common: Common,
        /// auto, basic, medium, high or ultra.
        setting: Option<GraphicsSetting>,
    },
}

#[derive(Args)]
struct Common {
    /// Install directory. Default: $XDG_DATA_HOME/zwift.
    #[arg(long)]
    dir: Option<PathBuf>,
    #[arg(long, default_value = zwift_bootstrap::DEFAULT_BASE_URL)]
    base_url: String,
}

#[derive(Args)]
struct SyncArgs {
    #[command(flatten)]
    common: Common,
    #[arg(long, default_value = "8")]
    jobs: NonZeroUsize,
    /// Also show the progress in a window. The window opens only when there is something to
    /// download. Exit code 130 when the user cancels.
    #[arg(long)]
    window: bool,
}

fn main() -> ExitCode {
    let mut progress = Progress::new();
    let result = run(Cli::parse().command, &mut progress);
    progress.end_line();
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            if let Error::Files(failed) = &error {
                for (path, cause) in failed.iter().take(20) {
                    eprintln!("  {path}: {cause}");
                }
            }
            ExitCode::from(2)
        }
    }
}

/// Exit codes: 0 ok or up to date, 1 `verify` found problems, 2 any error, 130 cancelled in
/// the progress window.
fn run(command: Command, progress: &mut Progress) -> Result<ExitCode, Error> {
    match command {
        Command::Check(common) => {
            let status = open(&common, FIXED_JOBS)?.check(&mut |event| progress.on_event(event))?;
            progress.end_line();
            match status {
                Status::Current { version } => println!("local {version}, server {version} (up to date)"),
                Status::UpdateAvailable { local, server, mandatory, plan } => {
                    let local = local.map_or_else(|| "none".to_owned(), |v| v.to_string());
                    let mandatory = if mandatory { ", mandatory" } else { "" };
                    println!("local {local}, server {server} (update available{mandatory})");
                    println!(
                        "fetch {} files ({}), delete {} files",
                        plan.fetch_files,
                        human(plan.fetch_bytes),
                        plan.delete_files
                    );
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Install(args) => sync(&args, SyncMode::Install, progress),
        Command::Update(args) => sync(&args, SyncMode::Update, progress),
        Command::Verify(common) => {
            let report = open(&common, FIXED_JOBS)?.verify(&mut |event| progress.on_event(event))?;
            progress.end_line();
            for problem in &report.problems {
                let what = match problem.kind {
                    ProblemKind::Missing => "missing".to_owned(),
                    ProblemKind::WrongLength { expected, actual } => {
                        format!("length {actual}, expected {expected}")
                    }
                    ProblemKind::WrongCrc { expected, actual } => format!("crc {actual}, expected {expected}"),
                };
                println!("{}: {what}", problem.path);
            }
            println!("{} files checked, {} problems", report.checked, report.problems.len());
            Ok(if report.problems.is_empty() { ExitCode::SUCCESS } else { ExitCode::from(1) })
        }
        Command::Launch { common, extra } => {
            let wine = env::var_os("WINE").filter(|w| !w.is_empty()).unwrap_or_else(|| "wine".into());
            let wine_prefix = env::var_os("WINEPREFIX")
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| xdg_dir("XDG_DATA_HOME", ".local/share").join("zwift-wine"));
            let bootstrapper = open(&common, FIXED_JOBS)?;
            let ble_socket = start_ble_bridge();
            let on_event = &mut |event| progress.on_event(event);
            match bootstrapper.launch(&wine, &wine_prefix, ble_socket.as_deref(), &extra, on_event)? {}
        }
        Command::Graphics { common, setting } => {
            let bootstrapper = open(&common, FIXED_JOBS)?;
            match setting {
                Some(setting) => bootstrapper.set_graphics_setting(setting)?,
                None => println!("{}", bootstrapper.graphics_setting()?),
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn start_ble_bridge() -> Option<PathBuf> {
    let Some(runtime) = env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) else {
        eprintln!("warning: XDG_RUNTIME_DIR is not set, so Bluetooth LE sensors are off");
        return None;
    };
    let socket = PathBuf::from(runtime).join("zwift-bootstrap-ble.sock");
    let helper = match env::current_exe() {
        Ok(exe) => exe.with_file_name("zwift-bootstrap-ble"),
        Err(e) => {
            eprintln!("warning: cannot find the Bluetooth LE bridge ({e}), so Bluetooth LE sensors are off");
            return None;
        }
    };
    let started = std::process::Command::new(&helper)
        .arg(&socket)
        .arg(format!("--parent={}", std::process::id()))
        .stdin(std::process::Stdio::null())
        .spawn();
    match started {
        Ok(_) => Some(socket),
        Err(e) => {
            eprintln!("warning: cannot start {} ({e}), so Bluetooth LE sensors are off", helper.display());
            None
        }
    }
}

fn sync(args: &SyncArgs, mode: SyncMode, progress: &mut Progress) -> Result<ExitCode, Error> {
    let bootstrapper = open(&args.common, args.jobs)?;
    let outcome = if args.window {
        match window::sync(bootstrapper, mode, progress) {
            window::End::Finished(result) => result?,
            window::End::Cancelled => {
                progress.note("cancelled");
                return Ok(ExitCode::from(130));
            }
        }
    } else {
        bootstrapper.sync(mode, &mut |event| progress.on_event(event))?
    };
    progress.end_line();
    match outcome {
        SyncOutcome::Current { version } => println!("up to date at {version}"),
        SyncOutcome::Updated { from, to, report } => {
            let from = from.map_or_else(String::new, |v| format!(" from {v}"));
            println!(
                "installed {to}{from}: {} downloaded ({}), {} reused, {} deleted",
                report.downloaded_files,
                human(report.downloaded_bytes),
                report.reused_files,
                report.deleted_files
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `$XDG_DATA_HOME/zwift`, else `~/.local/share/zwift`; staging `$XDG_CACHE_HOME/zwift/downloads`,
/// else `~/.cache/zwift/downloads`. Empty or relative XDG values are ignored, per the spec.
fn config(common: &Common, jobs: NonZeroUsize) -> Config {
    Config {
        install_dir: common.dir.clone().unwrap_or_else(|| xdg_dir("XDG_DATA_HOME", ".local/share").join("zwift")),
        staging_dir: xdg_dir("XDG_CACHE_HOME", ".cache").join("zwift").join("downloads"),
        base_url: common.base_url.clone(),
        jobs,
    }
}

fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    env::var_os(var).map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| {
        env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(fallback)
    })
}

fn open(common: &Common, jobs: NonZeroUsize) -> Result<Bootstrapper, Error> {
    Bootstrapper::new(config(common, jobs))
}

fn human(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let b = bytes as f64;
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.1} KiB", b / KIB),
        1_048_576..1_073_741_824 => format!("{:.1} MiB", b / (KIB * KIB)),
        _ => format!("{:.2} GiB", b / (KIB * KIB * KIB)),
    }
}

/// Bytes per second over the samples of the last 3 s.
#[derive(Default)]
struct Rate {
    samples: VecDeque<(Instant, u64)>,
    per_second: f64,
}

impl Rate {
    const WINDOW: Duration = Duration::from_secs(3);

    fn record(&mut self, now: Instant, bytes: u64) {
        self.samples.push_back((now, bytes));
        while self.samples.front().is_some_and(|(t, _)| now - *t > Self::WINDOW) {
            self.samples.pop_front();
        }
        self.per_second = match (self.samples.front(), self.samples.back()) {
            (Some((t0, b0)), Some((t1, b1))) if t1 > t0 => {
                b1.saturating_sub(*b0) as f64 / (*t1 - *t0).as_secs_f64()
            }
            _ => 0.0,
        };
    }

    fn clear(&mut self) {
        *self = Rate::default();
    }
}

/// One `\r`-rewritten line on a TTY (`IsTerminal`), else a plain line every 5 s:
/// `staging  1234/19777 files  1.21/4.83 GiB  42.0 MiB/s`. Retries and warnings go on
/// their own lines. The numbers come from each `Tally` snapshot; only the rate keeps
/// history.
struct Progress {
    tty: bool,
    phase: Option<Phase>,
    line_open: bool,
    last_plain: Option<Instant>,
    rate: Rate,
}

impl Progress {
    const PLAIN_EVERY: Duration = Duration::from_secs(5);

    fn new() -> Progress {
        Progress {
            tty: std::io::stderr().is_terminal(),
            phase: None,
            line_open: false,
            last_plain: None,
            rate: Rate::default(),
        }
    }

    fn on_event(&mut self, event: Event) {
        match event {
            Event::Phase(phase) => {
                self.phase = Some(phase);
                self.rate.clear();
                let text = match phase {
                    Phase::CheckingVersion => "checking version",
                    Phase::FetchingManifest => "fetching manifest",
                    Phase::Staging => "staging",
                    Phase::Committing => "committing",
                    Phase::Verifying => "verifying",
                };
                self.show(text.to_owned(), true);
            }
            Event::Progress(tally) => {
                let line = self.progress_line(tally);
                let done = tally.files_done == tally.files_total;
                self.show(line, done);
            }
            Event::Retry { path, attempt, cause } => self.note(&format!("retry {attempt} {path}: {cause}")),
            Event::Warning(text) => self.note(&format!("warning: {text}")),
            Event::Graphics(choice) => self.note(&format!("graphics: {choice}")),
        }
    }

    fn progress_line(&mut self, tally: Tally) -> String {
        let measured = match self.phase {
            Some(Phase::Verifying) => tally.bytes_done,
            _ => tally.bytes_downloaded,
        };
        self.rate.record(Instant::now(), measured);
        let label = if self.phase == Some(Phase::Verifying) { "verifying" } else { "staging" };
        format!(
            "{label}  {}/{} files  {}  {}/s",
            tally.files_done,
            tally.files_total,
            byte_pair(tally.bytes_done, tally.bytes_total),
            human(self.rate.per_second as u64)
        )
    }

    /// `force` prints a plain line even inside the 5 s interval (phase changes, the end).
    fn show(&mut self, line: String, force: bool) {
        let mut err = std::io::stderr().lock();
        if self.tty {
            let _ = write!(err, "\r\x1b[K{line}");
            let _ = err.flush();
            self.line_open = true;
        } else if force || self.last_plain.is_none_or(|t| t.elapsed() >= Self::PLAIN_EVERY) {
            let _ = writeln!(err, "{line}");
            self.last_plain = Some(Instant::now());
        }
    }

    fn note(&mut self, text: &str) {
        self.end_line();
        eprintln!("{text}");
    }

    fn end_line(&mut self) {
        if self.line_open {
            eprintln!();
            self.line_open = false;
        }
    }
}

/// `1.21/4.83 GiB`: both numbers in the unit of the total.
fn byte_pair(done: u64, total: u64) -> String {
    let total_text = human(total);
    let (number, unit) = total_text.split_once(' ').unwrap_or((&total_text, ""));
    let decimals = number.split_once('.').map_or(0, |(_, d)| d.len());
    let scale = match unit {
        "KiB" => 1024f64,
        "MiB" => 1024f64.powi(2),
        "GiB" => 1024f64.powi(3),
        _ => 1.0,
    };
    format!("{:.*}/{number} {unit}", decimals, done as f64 / scale)
}
