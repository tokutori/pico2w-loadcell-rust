//! Application transitions and data ownership; widgets live in ui.
#[cfg(test)]
mod tests;
mod ui;
use crate::{COLORS, axis, balance, history, transport};
use balance::{trace as balance_trace, ui as balance_ui};
use eframe::egui;
use egui_plot::{Legend, Line, Plot, Points};
use history::{History, TareState};
use serial_plotter_core::{Channel, Fault, Measurement, Metric};
use std::time::{Duration, Instant};
use transport::{Event, Session};

struct VerticalAxis {
    automatic: bool,
    range: axis::Range,
    lower: String,
    upper: String,
    editing: bool,
    error: Option<&'static str>,
}

impl Default for VerticalAxis {
    fn default() -> Self {
        Self {
            automatic: true,
            range: axis::Range::default(),
            lower: "-1".into(),
            upper: "1".into(),
            editing: false,
            error: None,
        }
    }
}

impl VerticalAxis {
    fn controls(&mut self, ui: &mut egui::Ui, label: &str) {
        ui.horizontal_wrapped(|ui| {
            ui.strong(label);
            if ui.checkbox(&mut self.automatic, "Auto Y").changed() {
                self.editing = false;
                self.error = None;
            }
            ui.label("Lower");
            let lower = ui.add(egui::TextEdit::singleline(&mut self.lower).desired_width(105.0));
            ui.label("Upper");
            let upper = ui.add(egui::TextEdit::singleline(&mut self.upper).desired_width(105.0));
            self.editing |= lower.changed() || upper.changed();
            let enter = (lower.lost_focus() || upper.lost_focus())
                && ui.input(|input| input.key_pressed(egui::Key::Enter));
            if ui.button("Apply Y").clicked() || enter {
                match axis::Range::parse(&self.lower, &self.upper) {
                    Ok(range) => {
                        self.range = range;
                        self.automatic = false;
                        self.editing = false;
                        self.error = None;
                    }
                    Err(error) => self.error = Some(error),
                }
            }
            if let Some(error) = self.error {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            }
        });
    }

    fn sync_labels(&mut self) {
        if !self.editing {
            self.lower = self.range.min.to_string();
            self.upper = self.range.max.to_string();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Balance,
    Graphs,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TransportChoice {
    Usb,
    Ble,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConnectionPhase {
    Opening,
    Ready,
    Stopping,
}

/// Live reception and demo data are mutually exclusive. A demo owns the entire
/// real panel so zero offsets, dirty state and settings path are restored together.
enum Source {
    Offline,
    Live {
        session: Session,
        phase: ConnectionPhase,
    },
    Demo {
        saved: Box<balance_ui::Panel>,
    },
}
impl Source {
    fn session(&self) -> Option<&Session> {
        match self {
            Self::Live { session, .. } => Some(session),
            _ => None,
        }
    }
    fn is_demo(&self) -> bool {
        matches!(self, Self::Demo { .. })
    }
    fn ready(&self) -> bool {
        matches!(
            self,
            Self::Live {
                phase: ConnectionPhase::Ready,
                ..
            }
        )
    }
}

pub(crate) struct Plotter {
    balance: balance_ui::Panel,
    view: View,
    pc_graphs: bool,
    transport: TransportChoice,
    ble_address: String,
    port: String,
    ports: Vec<String>,
    baud: u32,
    source: Source,
    status: String,
    started: Instant,
    history: History,
    visible: [bool; 4],
    window_seconds: f64,
    paused_at: Option<f64>,
    reframe_time: bool,
    vertical_axes: [VerticalAxis; 3],
    follow_log: bool,
}

impl Plotter {
    pub(crate) fn initialize(&mut self, demo: bool, cg_demo: bool) {
        self.balance.load_default();
        if cg_demo {
            self.load_cg_demo();
        } else if demo {
            self.load_demo();
        }
    }

    pub(crate) fn new(port: String, demo: bool) -> Self {
        let mut app = Self {
            balance: balance_ui::Panel::default(),
            view: View::Balance,
            pc_graphs: true,
            transport: TransportChoice::Usb,
            ble_address: String::new(),
            port,
            ports: Vec::new(),
            baud: 115_200,
            source: Source::Offline,
            status: "Disconnected. Close other serial monitors before connecting.".into(),
            started: Instant::now(),
            history: History::default(),
            visible: [true, false, false, false],
            window_seconds: 60.0,
            paused_at: None,
            reframe_time: true,
            vertical_axes: std::array::from_fn(|_| VerticalAxis::default()),
            follow_log: true,
        };
        app.refresh_ports();
        if demo {
            app.load_demo();
        }
        app
    }

    fn refresh_ports(&mut self) {
        match serialport::available_ports() {
            Ok(ports) => {
                self.ports = ports.into_iter().map(|port| port.port_name).collect();
                self.ports.sort();
            }
            Err(error) => {
                self.status = format!("Cannot list ports: {error}. You can enter a port manually.")
            }
        }
    }

    fn connect(&mut self) {
        if self.source.session().is_some() {
            return;
        }
        if self.transport == TransportChoice::Usb && (self.port.trim().is_empty() || self.baud == 0)
        {
            self.status = "Enter a port and a positive baud rate.".into();
            return;
        }
        let started = Instant::now();
        let connection = if self.transport == TransportChoice::Ble {
            Session::start_ble(self.ble_address.trim().to_owned())
        } else {
            Session::start(self.port.trim().to_owned(), self.baud)
        };
        match connection {
            Ok(session) => {
                self.leave_demo();
                self.balance.begin_session();
                self.pc_graphs = true;
                self.source = Source::Live {
                    session,
                    phase: ConnectionPhase::Opening,
                };
                self.started = started;
                self.history = History::default();
                self.balance.trace = balance_trace::Trace::default();
                self.paused_at = None;
                self.reframe_time = true;
                self.status = if self.transport == TransportChoice::Ble {
                    "Starting BLE discovery...".into()
                } else {
                    format!("Opening {}...", self.port)
                };
            }
            Err(error) => self.status = format!("Cannot start reader: {error}"),
        }
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Ready(link) => {
                if let Source::Live { phase, .. } = &mut self.source
                    && *phase != ConnectionPhase::Stopping
                {
                    *phase = ConnectionPhase::Ready;
                    self.status = link.to_string();
                }
            }
            Event::Status(status) => {
                if !matches!(
                    self.source,
                    Source::Live {
                        phase: ConnectionPhase::Stopping,
                        ..
                    }
                ) {
                    self.status = status.to_string();
                }
            }
            Event::Gap => self.history.transport_gap(),
            Event::Bytes { at, bytes } => {
                let received = at.saturating_duration_since(self.started).as_secs_f64();
                self.history.ingest(received, &bytes);
                if self.source.ready() {
                    // Freshness is evaluated when data is consumed, not when a
                    // delayed queue entry was produced by the transport worker.
                    self.balance
                        .observe(&self.history, self.started.elapsed().as_secs_f64());
                }
            }
        }
    }

    fn drain(&mut self) {
        // Own the small event batch before mutating application state.
        let Some(session) = self.source.session() else {
            return;
        };
        let events: Vec<_> = session.events.try_iter().take(64).collect();
        for event in events {
            self.handle_event(event);
        }
        let tail: Vec<_> = match self.source.session() {
            Some(session) if session.is_finished() => session.events.try_iter().collect(),
            _ => return,
        };
        for event in tail {
            self.handle_event(event);
        }
        if let Source::Live { session, .. } = std::mem::replace(&mut self.source, Source::Offline) {
            self.balance.end_session();
            self.status = match session.finish() {
                Ok(()) => "Disconnected. Data retained; reconnect starts a new session.".into(),
                Err(error) => error.to_string(),
            };
        }
    }

    fn disconnect(&mut self) {
        if let Source::Live { session, phase } = &mut self.source {
            session.request_stop();
            *phase = ConnectionPhase::Stopping;
            self.balance.end_session();
            self.status = "Disconnecting...".into();
        }
    }

    fn enter_demo(&mut self) {
        if !self.source.is_demo() {
            let saved = Box::new(std::mem::take(&mut self.balance));
            self.balance.config = saved.config.clone();
            self.source = Source::Demo { saved };
        }
        self.balance.end_session();
        self.paused_at = None;
        self.reframe_time = true;
    }

    fn leave_demo(&mut self) {
        if self.source.is_demo()
            && let Source::Demo { saved } = std::mem::replace(&mut self.source, Source::Offline)
        {
            self.balance = *saved;
            self.balance.end_session();
            self.balance.trace = balance_trace::Trace::default();
            self.history = History::default();
            self.started = Instant::now();
            self.paused_at = None;
            self.reframe_time = true;
            self.status = "Demo ended. Real settings restored; disconnected.".into();
        }
    }

    fn end_time(&self) -> f64 {
        self.paused_at
            .unwrap_or_else(|| match self.source.session() {
                Some(_) => self.started.elapsed().as_secs_f64(),
                None => self.history.last_time,
            })
    }

    fn measurement_time(&self) -> f64 {
        if self.source.is_demo() {
            self.history.last_time
        } else {
            self.started.elapsed().as_secs_f64()
        }
    }

    fn sample_counts(&self) -> [usize; 4] {
        self.history
            .valid_samples_last_second(self.measurement_time())
    }

    fn load_demo(&mut self) {
        if self.source.session().is_some() {
            return;
        }
        self.enter_demo();
        self.balance.end_session();
        self.pc_graphs = false;
        self.view = View::Graphs;
        self.balance.trace = balance_trace::Trace::default();
        self.history = History::default();
        self.paused_at = None;
        self.reframe_time = true;
        self.status = "DEMO | sample log, no serial connection".into();
        self.history
            .ingest(0.0, b"DEMO: sample log; no hardware connected\r\n");
        let raw_values = [
            -155256, -155291, -155340, -1, -155366, -1, -1, -1, -982399, -1509065, -1585505,
            -1487427, -154458, -155438, -1, -155495, -1, -155475, -155540, -1, -155573, -155571,
            -155587,
        ];
        for (i, raw) in raw_values.into_iter().enumerate() {
            let delta = raw + 141513;
            let line = format!(
                "LC1 raw={raw} delta={delta} kg~={:.3}\r\nLC2 status=timeout\r\nLC3 status=timeout\r\nLC4 raw=0 delta=0 kg~=0.000\r\n",
                f64::from(delta) / 77309.0
            );
            self.history.ingest(i as f64 * 0.65, line.as_bytes());
        }
    }

    fn load_cg_demo(&mut self) {
        if self.source.session().is_some() {
            return;
        }
        self.enter_demo();
        self.balance.end_session();
        self.balance.trace = balance_trace::Trace::default();
        self.history = History::default();
        self.view = View::Balance;
        self.status = "CG DEMO | simulated static 100 kg load; no serial connection".into();
        self.balance.config = balance::Config::default();
        self.balance.config.layout = balance::Layout {
            points: [[200.0, 0.0], [-200.0, 0.0], [320.0, 610.0], [-280.0, 590.0]],
            channels: Default::default(),
        };
        for (i, cell) in self.balance.config.cells.iter_mut().enumerate() {
            cell.zero_raw = Some(-100_000.0);
            cell.counts_per_kg = if i == 0 { -1000.0 } else { 1000.0 };
            cell.calibrated = true;
        }
        for n in 0..30 {
            for i in 0..4 {
                let drift = (24_i32 - n).max(0) * 400;
                let raw = -100_000
                    + match i {
                        0 => -10_000 - drift,
                        1 => 20_000 - drift,
                        _ => (i + 1) * 10_000,
                    };
                self.history.ingest(
                    f64::from(n) * 0.1,
                    format!("LC{} raw={raw} status=no_tare\n", i + 1).as_bytes(),
                );
            }
            self.balance.observe(&self.history, self.history.last_time);
        }
    }
}

impl Drop for Plotter {
    fn drop(&mut self) {
        if let Source::Live { session, .. } = std::mem::replace(&mut self.source, Source::Offline) {
            session.request_stop();
            let _ = session.finish();
        }
    }
}
