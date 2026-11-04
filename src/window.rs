//! `install --window` and `update --window`: the sync runs on a worker thread, and the
//! main thread shows its events in a small window and on stderr. The window opens only
//! when the sync downloads, so a start that is up to date shows no window.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use eframe::egui;
use zwift_bootstrap::{Bootstrapper, Error, Event, Phase, SyncMode, SyncOutcome, Tally};

use crate::{Progress, byte_pair, human};

pub enum End {
    Finished(Result<SyncOutcome, Error>),
    Cancelled,
}

enum Msg {
    Event(Event),
    Done(Result<SyncOutcome, Error>),
}

enum State {
    Working { phase: Phase, tally: Option<Tally> },
    /// Shown until the user closes the window.
    Failed(Error),
    Succeeded(SyncOutcome),
    Cancelled,
}

struct Session<'p> {
    rx: Receiver<Msg>,
    progress: &'p mut Progress,
    state: State,
}

pub fn sync(bootstrapper: Bootstrapper, mode: SyncMode, progress: &mut Progress) -> End {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = bootstrapper.sync(mode, &mut |event| {
            let _ = tx.send(Msg::Event(event));
        });
        let _ = tx.send(Msg::Done(result));
    });
    let mut session =
        Session { rx, progress, state: State::Working { phase: Phase::CheckingVersion, tally: None } };
    while matches!(session.state, State::Working { phase: Phase::CheckingVersion, .. }) {
        session.receive();
    }
    if matches!(session.state, State::Working { .. }) {
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("Zwift")
                .with_app_id("zwift-bootstrap")
                .with_inner_size([460.0, 150.0])
                .with_resizable(false),
            ..Default::default()
        };
        let opened =
            eframe::run_native("zwift-bootstrap", options, Box::new(|_| Ok(Box::new(Window(&mut session)))));
        if let Err(e) = opened {
            session.progress.note(&format!("warning: cannot open the progress window ({e})"));
        }
    }
    session.finish()
}

impl Session<'_> {
    fn receive(&mut self) {
        let msg = self.rx.recv().expect("the sync thread ended without a result");
        self.apply(msg);
    }

    fn apply(&mut self, msg: Msg) {
        match msg {
            Msg::Event(event) => {
                if let State::Working { phase, tally } = &mut self.state {
                    match &event {
                        Event::Phase(p) => *phase = *p,
                        Event::Progress(t) => *tally = Some(*t),
                        Event::Retry { .. } | Event::Warning(_) => {}
                    }
                }
                self.progress.on_event(event);
            }
            Msg::Done(Ok(outcome)) => self.state = State::Succeeded(outcome),
            Msg::Done(Err(error)) => self.state = State::Failed(error),
        }
    }

    /// Text mode until the sync ends, when the window could not open or closed early.
    fn finish(mut self) -> End {
        loop {
            match self.state {
                State::Working { .. } => self.receive(),
                State::Succeeded(outcome) => return End::Finished(Ok(outcome)),
                State::Failed(error) => return End::Finished(Err(error)),
                State::Cancelled => return End::Cancelled,
            }
        }
    }
}

struct Window<'s, 'p>(&'s mut Session<'p>);

impl eframe::App for Window<'_, '_> {
    // eframe calls `logic` but not `ui` while the window is hidden, for example on another
    // workspace. The channel must drain there, so that stderr and the end do not stall.
    fn logic(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        let session = &mut *self.0;
        loop {
            match session.rx.try_recv() {
                Ok(msg) => session.apply(msg),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if matches!(session.state, State::Working { .. }) {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    break;
                }
            }
        }
        if matches!(session.state, State::Working { .. }) && ctx.input(|i| i.viewport().close_requested()) {
            session.state = State::Cancelled;
        }
        if matches!(session.state, State::Succeeded(_) | State::Cancelled) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(Duration::from_millis(100));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        let session = &mut *self.0;
        egui::CentralPanel::default_margins().show(ui, |ui| match &session.state {
            State::Working { phase, tally } => {
                ui.heading(heading(*phase));
                ui.add_space(8.0);
                ui.add(egui::ProgressBar::new(tally.map_or(0.0, fraction)));
                if let Some(tally) = tally {
                    ui.label(status_line(*tally, session.progress.rate.per_second));
                }
                if bottom_right_button(ui, "Cancel") {
                    session.state = State::Cancelled;
                }
            }
            State::Failed(error) => {
                ui.heading("The download failed.");
                ui.add_space(8.0);
                ui.label(error.to_string());
                if bottom_right_button(ui, "Close") {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            State::Succeeded(_) | State::Cancelled => {}
        });
    }
}

fn bottom_right_button(ui: &mut egui::Ui, text: &str) -> bool {
    ui.with_layout(egui::Layout::bottom_up(egui::Align::Max), |ui| ui.button(text).clicked()).inner
}

fn heading(phase: Phase) -> &'static str {
    match phase {
        Phase::CheckingVersion => "Checking the version",
        Phase::FetchingManifest => "Downloading the file list",
        Phase::Staging => "Downloading Zwift",
        Phase::Committing => "Installing",
        Phase::Verifying => "Checking the files",
    }
}

fn fraction(tally: Tally) -> f32 {
    if tally.bytes_total == 0 { 1.0 } else { (tally.bytes_done as f64 / tally.bytes_total as f64) as f32 }
}

/// `2.72/4.78 GiB  56%  38.0 MiB/s  about 1 min left`. No time while the rate is 0.
fn status_line(tally: Tally, rate: f64) -> String {
    let percent = (tally.bytes_done * 100).checked_div(tally.bytes_total).unwrap_or(100);
    let mut line = format!(
        "{}  {percent}%  {}/s",
        byte_pair(tally.bytes_done, tally.bytes_total),
        human(rate as u64)
    );
    if rate > 0.0 {
        let seconds = tally.bytes_total.saturating_sub(tally.bytes_done) as f64 / rate;
        if seconds < 60.0 {
            line.push_str("  less than 1 min left");
        } else {
            line.push_str(&format!("  about {:.0} min left", seconds / 60.0));
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;

    fn tally(bytes_done: u64, bytes_total: u64) -> Tally {
        Tally { bytes_done, bytes_total, ..Tally::default() }
    }

    #[test]
    fn status_line_shows_bytes_percent_rate_and_time_left() {
        assert_eq!(
            status_line(tally(GIB, 4 * GIB), 16.0 * MIB as f64),
            "1.00/4.00 GiB  25%  16.0 MiB/s  about 3 min left"
        );
        assert_eq!(
            status_line(tally(4 * GIB - 100 * MIB, 4 * GIB), 10.0 * MIB as f64),
            "3.90/4.00 GiB  97%  10.0 MiB/s  less than 1 min left"
        );
    }

    #[test]
    fn status_line_has_no_time_left_while_the_rate_is_zero() {
        assert_eq!(status_line(tally(GIB, 4 * GIB), 0.0), "1.00/4.00 GiB  25%  0 B/s");
    }

    #[test]
    fn percent_shows_100_only_when_all_bytes_are_done() {
        assert!(status_line(tally(999, 1000), 0.0).contains("  99%  "));
        assert!(status_line(tally(0, 0), 0.0).contains("  100%  "));
    }
}
