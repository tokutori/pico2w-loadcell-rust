#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod axis;
mod balance;
mod balance_settings;
#[cfg(test)]
mod balance_tests;
mod balance_trace;
mod balance_ui;
mod ble;
mod history;
mod transport;

use eframe::egui;
use egui_plot::{Legend, Line, Plot, Points};
use history::{History, TareState};
use serial_plotter_core::{Channel, Fault, Measurement, Metric};
use std::time::{Duration, Instant};
use transport::{Event, Session};

const COLORS: [egui::Color32; 4] = [
    egui::Color32::from_rgb(60, 190, 230),
    egui::Color32::from_rgb(250, 185, 70),
    egui::Color32::from_rgb(140, 215, 115),
    egui::Color32::from_rgb(220, 125, 215),
];

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

struct Plotter {
    balance: balance_ui::Panel,
    view: View,
    demo_balance_backup: Option<balance::Config>,
    pc_graphs: bool,
    use_ble: bool,
    ble_address: String,
    port: String,
    ports: Vec<String>,
    baud: u32,
    session: Option<Session>,
    status: String,
    started: Instant,
    history: History,
    visible: [bool; 4],
    window_seconds: f64,
    paused_at: Option<f64>,
    reframe_time: bool,
    vertical_axes: [VerticalAxis; 3],
    follow_log: bool,
    demo: bool,
}

impl Plotter {
    fn new(port: String, demo: bool) -> Self {
        let mut app = Self {
            balance: balance_ui::Panel::default(),
            view: View::Balance,
            demo_balance_backup: None,
            pc_graphs: true,
            use_ble: false,
            ble_address: String::new(),
            port,
            ports: Vec::new(),
            baud: 115_200,
            session: None,
            status: "Disconnected. Close other serial monitors before connecting.".into(),
            started: Instant::now(),
            history: History::default(),
            visible: [true, false, false, false],
            window_seconds: 60.0,
            paused_at: None,
            reframe_time: true,
            vertical_axes: std::array::from_fn(|_| VerticalAxis::default()),
            follow_log: true,
            demo: false,
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
        if !self.use_ble && (self.port.trim().is_empty() || self.baud == 0) {
            self.status = "Enter a port and a positive baud rate.".into();
            return;
        }
        let started = Instant::now();
        let connection = if self.use_ble {
            Session::start_ble(self.ble_address.trim().to_owned())
        } else {
            Session::start(self.port.trim().to_owned(), self.baud)
        };
        match connection {
            Ok(session) => {
                if let Some(config) = self.demo_balance_backup.take() {
                    self.balance.config = config;
                }
                self.balance.begin_session();
                self.pc_graphs = true;
                self.session = Some(session);
                self.started = started;
                self.history = History::default();
                self.balance.trace = balance_trace::Trace::default();
                self.paused_at = None;
                self.reframe_time = true;
                self.demo = false;
                self.status = if self.use_ble {
                    "Starting BLE discovery...".into()
                } else {
                    format!("Opening {}...", self.port)
                };
            }
            Err(error) => self.status = format!("Cannot start reader: {error}"),
        }
    }

    fn drain(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        // A per-frame budget keeps controls responsive even with a fast sender.
        for _ in 0..64 {
            match session.events.try_recv() {
                Ok(Event::Opened) => {
                    self.status =
                        format!("Connected to {} | DTR on | PC zero/calibration", self.port)
                }
                Ok(Event::Status(status)) => self.status = status,
                Ok(Event::Gap) => self.history.transport_gap(),
                Ok(Event::Bytes { at, bytes }) => {
                    let now = at.duration_since(self.started).as_secs_f64();
                    self.history.ingest(now, &bytes);
                    self.balance.observe(&self.history, now);
                }
                Err(_) => break,
            }
        }
        if session.is_finished() {
            // A finished reader cannot enqueue any more data. Drain its tail.
            for event in session.events.try_iter() {
                match event {
                    Event::Bytes { at, bytes } => {
                        self.history
                            .ingest(at.duration_since(self.started).as_secs_f64(), &bytes);
                        self.balance.observe(&self.history, self.history.last_time);
                    }
                    Event::Gap => self.history.transport_gap(),
                    _ => {}
                }
            }
            if let Some(session) = self.session.take() {
                self.balance.end_session();
                self.status = match session.finish() {
                    Ok(()) => "Disconnected. Data retained; reconnect starts a new session.".into(),
                    Err(error) => error,
                };
            }
        }
    }

    fn end_time(&self) -> f64 {
        self.paused_at.unwrap_or_else(|| match self.session {
            Some(_) => self.started.elapsed().as_secs_f64(),
            None => self.history.last_time,
        })
    }

    fn measurement_time(&self) -> f64 {
        if self.demo {
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
        self.balance.end_session();
        self.pc_graphs = false;
        self.view = View::Graphs;
        self.balance.trace = balance_trace::Trace::default();
        self.history = History::default();
        self.demo = true;
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
        self.balance.end_session();
        self.balance.trace = balance_trace::Trace::default();
        if self.demo_balance_backup.is_none() {
            self.demo_balance_backup = Some(self.balance.config.clone());
        }
        self.history = History::default();
        self.demo = true;
        self.view = View::Balance;
        self.status = "CG DEMO | simulated static 100 kg load; no serial connection".into();
        self.balance.config = balance::Config::default();
        self.balance.config.layout = balance::Layout {
            points: [[200.0, 0.0], [-200.0, 0.0], [320.0, 610.0], [-280.0, 590.0]],
            channels: [0, 1, 2, 3],
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

    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.heading("Load cell monitor");
            ui.separator();
            ui.add_enabled_ui(self.session.is_none(), |ui| {
                ui.selectable_value(&mut self.use_ble, false, "USB serial");
                ui.selectable_value(&mut self.use_ble, true, "BLE");
                if self.use_ble {
                    ui.label("BLE address (optional)");
                    ui.add(egui::TextEdit::singleline(&mut self.ble_address).desired_width(150.0))
                        .on_hover_text("Leave empty to connect when exactly one Loadcells board is found. Connect scans for 5 seconds.");
                } else {
                ui.label("Port");
                ui.add(egui::TextEdit::singleline(&mut self.port).desired_width(110.0));
                egui::ComboBox::from_id_salt("ports")
                    .selected_text("Available ports")
                    .show_ui(ui, |ui| {
                        for port in &self.ports {
                            ui.selectable_value(&mut self.port, port.clone(), port);
                        }
                    });
                if ui.button("Refresh").clicked() {
                    self.refresh_ports();
                }
                ui.label("Baud");
                ui.add(egui::DragValue::new(&mut self.baud).range(1..=4_000_000));
                }
                if ui.button("Connect").clicked() {
                    self.connect();
                }
            });
            if ui
                .add_enabled(self.session.is_some(), egui::Button::new("Disconnect"))
                .clicked()
                && let Some(session) = &self.session
            {
                session.request_stop();
                self.status = "Disconnecting...".into();
            }
        });
        ui.label(&self.status);
        ui.horizontal_wrapped(|ui| {
            ui.strong("Valid samples / last 1 s (PC received):");
            for (i, count) in self.sample_counts().into_iter().enumerate() {
                ui.colored_label(COLORS[i], format!("LC{}: {count}", i + 1));
            }
        });
        let now = self.measurement_time();
        self.balance
            .tare_controls(ui, &self.history, now, self.session.is_some(), self.demo);
        if self.history.tare != TareState::Unknown {
            ui.horizontal_wrapped(|ui| {
            ui.label(match self.history.tare {
                TareState::Unknown => "Legacy firmware tare: unknown",
                TareState::Running => "Legacy firmware tare in progress",
                TareState::Complete => "Legacy firmware tare finished",
            });
            for channel in Channel::ALL {
                if let Some(required) = self.history.tare_failures[channel.index()] {
                    ui.colored_label(egui::Color32::YELLOW, format!("{} tare failed ({required} valid readings required); reconnect unloaded", channel.label()));
                }
            }
        });
        }
        ui.horizontal_wrapped(|ui| {
            if self.view == View::Graphs {
                ui.label("Plot channels:");
                for channel in Channel::ALL {
                    ui.checkbox(
                        &mut self.visible[channel.index()],
                        egui::RichText::new(channel.label()).color(COLORS[channel.index()]),
                    );
                }
                ui.separator();
                ui.label("Window");
                if ui
                    .add(
                        egui::DragValue::new(&mut self.window_seconds)
                            .range(5.0..=3600.0)
                            .suffix(" s"),
                    )
                    .changed()
                {
                    self.reframe_time = true;
                }
                let mut follow = self.paused_at.is_none();
                if ui.checkbox(&mut follow, "Follow latest").changed() {
                    self.paused_at = if follow { None } else { Some(self.end_time()) };
                }
            }
            if ui.button("Clear history").clicked() {
                self.history.clear();
            }
            if ui
                .add_enabled(self.session.is_none(), egui::Button::new("Sample log"))
                .clicked()
            {
                self.load_demo();
            }
            if ui
                .add_enabled(self.session.is_none(), egui::Button::new("CG sample"))
                .clicked()
            {
                self.load_cg_demo();
            }
            ui.label(format!(
                "{} lines | {} parse/framing errors | {} firmware drops",
                self.history.line_count,
                self.history.errors,
                self.history
                    .dropped
                    .iter()
                    .copied()
                    .fold(0_u64, u64::saturating_add)
            ));
        });
        if self.view == View::Graphs {
            ui.checkbox(
                &mut self.pc_graphs,
                "PC calibrated graphs (same zero/span as CG)",
            );
            ui.label(if self.pc_graphs { "Retained raw history is converted using the current PC zero/span. Nominal spans need calibration." } else { "Legacy firmware values: raw-only firmware has no delta/kg output." });
            if self.paused_at.is_some() {
                ui.label(
                "Time view paused: drag / wheel to inspect retained history. Reception continues.",
            );
            }
            ui.label("Auto Y fits visible data. Lower / Upper + Apply Y fixes Y limits; turn off Auto Y to drag / zoom Y.");
            ui.horizontal_wrapped(|ui| {
                for channel in Channel::ALL {
                    let i = channel.index();
                    if !self.visible[i] {
                        continue;
                    }
                    let text = if self.pc_graphs {
                        let value = |metric| {
                            self.history.latest[i]
                                .and_then(|r| self.balance.value(i, r.measurement, metric))
                                .map(|v| format!("{v:.3}"))
                                .unwrap_or_else(|| "--".into())
                        };
                        format!(
                            "{} raw={} PC delta={} PC kg={}",
                            channel.label(),
                            value(Metric::Raw),
                            value(Metric::Delta),
                            value(Metric::Kg)
                        )
                    } else {
                        match self.history.latest[i].map(|reading| reading.measurement) {
                            Some(Measurement::Raw { raw }) => {
                                format!("{} raw={raw}", channel.label())
                            }
                            Some(Measurement::Sample { raw, delta, kg }) => {
                                format!(
                                    "{}  raw={raw}  delta={delta}  kg~={kg:.3}",
                                    channel.label()
                                )
                            }
                            Some(Measurement::Untared { raw }) => {
                                format!("{}  raw={raw}  no_tare", channel.label())
                            }
                            Some(Measurement::Fault(Fault::Timeout)) => {
                                format!("{}  timeout", channel.label())
                            }
                            Some(Measurement::Fault(Fault::Saturated)) => {
                                format!("{}  saturated", channel.label())
                            }
                            Some(Measurement::Fault(Fault::Protocol)) => {
                                format!("{}  protocol error", channel.label())
                            }
                            None => format!("{}  waiting for data", channel.label()),
                        }
                    };
                    ui.colored_label(COLORS[i], text);
                }
            });
        }
    }

    fn graphs(&mut self, ui: &mut egui::Ui) {
        let height = ((ui.available_height() - 105.0) / 3.0).max(100.0);
        let end = self.end_time().max(1.0);
        let start = (end - self.window_seconds).max(0.0);
        let end = end.max(start + 1.0);
        let follow = self.paused_at.is_none();
        let pc_graphs = self.pc_graphs;
        let balance = &self.balance;
        for (index, metric) in Metric::ALL.into_iter().enumerate() {
            let value = |channel: usize, m: Measurement| {
                if pc_graphs {
                    balance.value(channel, m, metric)
                } else {
                    m.value(metric)
                }
            };
            let label = if pc_graphs {
                match metric {
                    Metric::Raw => "raw (ADC counts)",
                    Metric::Delta => "PC delta (raw - PC zero)",
                    Metric::Kg => "PC kg (signed calibration)",
                }
            } else {
                metric.label()
            };
            let axis = &mut self.vertical_axes[index];
            ui.push_id(metric.label(), |ui| axis.controls(ui, label));
            let response = Plot::new(metric.label())
                .id(egui::Id::new(("measurement-plot", index)))
                .height(height)
                .legend(Legend::default())
                .x_axis_label("PC receive time (s)")
                .y_axis_label(label)
                .allow_drag([!follow, !axis.automatic])
                .allow_zoom([!follow, !axis.automatic])
                .allow_scroll([!follow, !axis.automatic])
                .allow_boxed_zoom(!follow && !axis.automatic)
                .allow_double_click_reset(false)
                .show(ui, |plot| {
                    plot.set_auto_bounds([false, false]);
                    if follow || self.reframe_time {
                        plot.set_plot_bounds_x(start..=end);
                    }
                    let (visible_start, visible_end) = if follow || self.reframe_time {
                        (start, end)
                    } else {
                        let bounds = plot.plot_bounds();
                        (bounds.min()[0], bounds.max()[0])
                    };
                    if axis.automatic {
                        let values = Channel::ALL
                            .into_iter()
                            .filter(|channel| self.visible[channel.index()])
                            .flat_map(|channel| {
                                self.history.samples[channel.index()]
                                    .iter()
                                    .map(move |s| (channel.index(), s))
                            })
                            .filter(|(_, sample)| {
                                sample.seconds >= visible_start && sample.seconds <= visible_end
                            })
                            .filter_map(|(i, sample)| value(i, sample.measurement));
                        let minimum_span = if matches!(metric, Metric::Kg) {
                            0.001
                        } else {
                            1.0
                        };
                        if let Some(range) = axis::Range::fit(values, minimum_span) {
                            axis.range = range;
                        }
                    }
                    plot.set_plot_bounds_y(axis.range.min..=axis.range.max);
                    for channel in Channel::ALL {
                        let i = channel.index();
                        if !self.visible[i] {
                            continue;
                        }
                        // Recompute from the current viewport, including after
                        // manual panning. Off-screen history need not be drawn.
                        let segments = if pc_graphs {
                            self.history
                                .segments_with(channel, visible_start, visible_end, |m| value(i, m))
                        } else {
                            self.history
                                .segments(channel, metric, visible_start, visible_end)
                        };
                        for points in segments {
                            if points.len() == 1 {
                                plot.points(
                                    Points::new(channel.label(), points)
                                        .color(COLORS[i])
                                        .radius(2.5_f32),
                                );
                            } else {
                                plot.line(
                                    Line::new(channel.label(), points)
                                        .color(COLORS[i])
                                        .width(1.5_f32),
                                );
                            }
                        }
                    }
                });
            if !follow && axis.automatic {
                // Plot input changes X after the closure; redraw once so the
                // visible samples and automatic Y bounds catch up to the pan.
                let changed_x = response.response.dragged()
                    || (response.response.hovered()
                        && ui.input(|input| {
                            input.smooth_scroll_delta != egui::Vec2::ZERO
                                || input.zoom_delta() != 1.0
                        }));
                if changed_x {
                    ui.ctx().request_repaint();
                }
            }
            if !axis.automatic {
                let bounds = response.transform.bounds();
                if let Some(range) = axis::Range::new(bounds.min()[1], bounds.max()[1]) {
                    axis.range = range;
                }
            }
            axis.sync_labels();
        }
        self.reframe_time = false;
    }
}

impl eframe::App for Plotter {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain();
        if self.session.is_some() || (!self.demo && self.sample_counts().iter().any(|n| *n > 0)) {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        egui::TopBottomPanel::top("controls").show(ctx, |ui| self.controls(ui));
        egui::TopBottomPanel::bottom("monitor")
            .resizable(true)
            .default_height(140.0)
            .min_height(90.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Serial monitor");
                    ui.checkbox(&mut self.follow_log, "Auto-scroll");
                    if ui.button("Copy retained log").clicked() {
                        ctx.copy_text(self.history.monitor_text());
                    }
                    ui.label("All channels / latest 2,000 lines");
                    if self.demo {
                        ui.colored_label(egui::Color32::YELLOW, "DEMO");
                    }
                });
                egui::ScrollArea::both()
                    .stick_to_bottom(self.follow_log)
                    .auto_shrink([false, false])
                    .show_rows(
                        ui,
                        ui.text_style_height(&egui::TextStyle::Monospace),
                        self.history.lines.len() + 1,
                        |ui, range| {
                            for i in range {
                                let text = match self.history.lines.get(i) {
                                    Some(line) => line.clone(),
                                    None => self.history.pending(),
                                };
                                ui.add(
                                    egui::Label::new(egui::RichText::new(text).monospace())
                                        .extend()
                                        .selectable(true),
                                );
                            }
                        },
                    );
            });
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.view, View::Balance, "Center of gravity");
                ui.selectable_value(&mut self.view, View::Graphs, "Sensor graphs");
                ui.selectable_value(
                    &mut self.view,
                    View::Settings,
                    "Sensor settings / calibration",
                );
            });
            let now = self.measurement_time();
            match self.view {
                View::Balance => {
                    egui::ScrollArea::vertical()
                        .id_salt("cg-page")
                        .show(ui, |ui| {
                            self.balance.show(
                                ui,
                                &self.history,
                                now,
                                self.session.is_some(),
                                self.demo,
                            );
                        });
                }
                View::Graphs => {
                    egui::ScrollArea::vertical()
                        .id_salt("raw-page")
                        .show(ui, |ui| self.graphs(ui));
                }
                View::Settings => {
                    egui::ScrollArea::vertical()
                        .id_salt("settings-page")
                        .show(ui, |ui| {
                            self.balance.show_settings(
                                ui,
                                &self.history,
                                now,
                                self.session.is_some(),
                                self.demo,
                            );
                        });
                }
            }
        });
    }
}

impl Drop for Plotter {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            session.request_stop();
            let _ = session.finish();
        }
    }
}

fn main() -> eframe::Result {
    let mut port = if cfg!(windows) {
        "COM5"
    } else {
        "/dev/ttyACM0"
    }
    .to_owned();
    let mut demo = false;
    let mut cg_demo = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--demo" => demo = true,
            "--cg-demo" => cg_demo = true,
            "--port" => {
                let Some(value) = args.next() else {
                    eprintln!("--port requires a value");
                    std::process::exit(2);
                };
                port = value;
            }
            "--help" | "-h" => {
                println!(
                    "serial-plotter [--port COM5] [--demo | --cg-demo]\nPorts are opened only when Connect is clicked."
                );
                return Ok(());
            }
            _ => {
                eprintln!("Unknown argument: {arg}");
                std::process::exit(2);
            }
        }
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 1000.0])
            .with_min_inner_size([850.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Load Cell Serial Plotter",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            let mut app = Plotter::new(port, demo);
            app.balance.load_default();
            if cg_demo {
                app.load_cg_demo();
            }
            Ok(Box::new(app))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_plot::{PlotBounds, PlotMemory};

    #[test]
    fn settings_page_exposes_coordinates_assignment_and_calibration() {
        let ctx = egui::Context::default();
        let mut app = Plotter::new(String::new(), false);
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1440.0, 1600.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    app.balance
                        .show_settings(ui, &app.history, 0.0, false, false);
                });
            },
        );
        fn collect(shape: &egui::Shape, text: &mut String) {
            match shape {
                egui::Shape::Text(t) => {
                    text.push_str(&t.galley.job.text);
                    text.push('\n');
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, text);
                    }
                }
                _ => {}
            }
        }
        let mut text = String::new();
        for clipped in &output.shapes {
            collect(&clipped.shape, &mut text);
        }
        for label in [
            "1. Measurement point coordinates",
            "2. Point to sensor assignment",
            "Upper right",
            "Upper left",
            "3. Sensor calibration",
            "Capture span",
            "Save settings",
        ] {
            assert!(text.contains(label), "missing settings control: {label}");
        }
    }

    #[test]
    fn cg_demo_and_headless_page_have_expected_position_and_keep_real_settings() {
        let mut app = Plotter::new(String::new(), false);
        let real = app.balance.config.clone();
        app.load_cg_demo();
        let cg = app
            .balance
            .config
            .calculate(&app.history, app.history.last_time)
            .unwrap();
        assert!((cg.x_mm + 36.0).abs() < 1e-8);
        assert!((cg.y_mm - 419.0).abs() < 1e-8);
        assert!((cg.total_kg - 100.0).abs() < 1e-8);
        assert_eq!(app.demo_balance_backup.as_ref(), Some(&real));
        let ctx = egui::Context::default();
        app.balance.config.layout = balance::Layout::default();
        assert_eq!(
            app.balance.config.layout.positions(),
            [
                [300.0, 0.0],
                [-300.0, 0.0],
                [300.0, 2000.0],
                [-300.0, 2000.0]
            ]
        );
        for width in [650.0, 850.0, 1440.0] {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 900.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        app.balance
                            .show(ui, &app.history, app.history.last_time, false, true);
                    });
                },
            );
            let xy = PlotMemory::load(&ctx, egui::Id::new("cg-layout")).unwrap();
            let tx = PlotMemory::load(&ctx, egui::Id::new(("cg-time", 0))).unwrap();
            let ty = PlotMemory::load(&ctx, egui::Id::new(("cg-time", 1))).unwrap();
            assert!(xy.bounds().min()[0] <= -300.0 && xy.bounds().max()[0] >= 300.0);
            assert!(xy.bounds().min()[1] <= -2000.0 && xy.bounds().max()[1] >= 0.0);
            let (xy_rect, tx_rect, ty_rect) = (
                *xy.transform().frame(),
                *tx.transform().frame(),
                *ty.transform().frame(),
            );
            assert!(ty_rect.top() >= tx_rect.bottom());
            if width >= 780.0 {
                assert!(tx_rect.left() >= xy_rect.right());
                assert!(xy_rect.height() > xy_rect.width());
            } else {
                assert!(tx_rect.top() >= xy_rect.bottom());
            }
        }
    }

    fn frame(app: &mut Plotter, ctx: &egui::Context) -> PlotBounds {
        frame_with_events(app, ctx, Vec::new())
    }

    fn frame_with_events(
        app: &mut Plotter,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> PlotBounds {
        let _ = ctx.run(
            egui::RawInput {
                events,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 900.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.graphs(ui));
            },
        );
        *PlotMemory::load(ctx, egui::Id::new(("measurement-plot", 0_usize)))
            .unwrap()
            .bounds()
    }

    #[test]
    fn fixed_y_survives_frames_while_time_follows_new_data() {
        let ctx = egui::Context::default();
        let mut app = Plotter::new(String::new(), true);
        app.window_seconds = 5.0;
        app.vertical_axes[0].automatic = false;
        app.vertical_axes[0].range = axis::Range::parse("-200000", "-100000").unwrap();
        for time in [20.0, 21.0, 100.0] {
            app.history.ingest(time, b"LC1 raw=-155000 delta=0 kg~=0\n");
            for _ in 0..3 {
                let bounds = frame(&mut app, &ctx);
                assert_eq!(bounds.min(), [time - 5.0, -200000.0]);
                assert_eq!(bounds.max(), [time, -100000.0]);
            }
        }
    }

    #[test]
    fn manual_y_zoom_is_not_reset_by_live_follow_or_repaint() {
        let ctx = egui::Context::default();
        let mut app = Plotter::new(String::new(), true);
        app.vertical_axes[0].automatic = false;
        app.vertical_axes[0].range = axis::Range::parse("-200000", "0").unwrap();
        let original = frame(&mut app, &ctx);
        let center = PlotMemory::load(&ctx, egui::Id::new(("measurement-plot", 0_usize)))
            .unwrap()
            .transform()
            .frame()
            .center();
        let zoomed = frame_with_events(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(center), egui::Event::Zoom(2.0)],
        );
        assert!(zoomed.height() < original.height());
        assert_eq!(zoomed.min()[0], original.min()[0]);
        assert_eq!(zoomed.max()[0], original.max()[0]);
        let repainted = frame(&mut app, &ctx);
        assert_eq!(zoomed.min(), repainted.min());
        assert_eq!(zoomed.max(), repainted.max());
    }

    #[test]
    fn auto_y_fits_current_window_and_paused_pan_keeps_older_history() {
        let ctx = egui::Context::default();
        let mut app = Plotter::new(String::new(), false);
        app.window_seconds = 5.0;
        app.history.ingest(1.0, b"LC1 raw=-1000000 delta=0 kg~=0\n");
        app.history.ingest(20.0, b"LC1 raw=-100 delta=0 kg~=0\n");
        let recent = frame(&mut app, &ctx);
        assert!(recent.min()[1] > -1000.0);
        assert!(recent.min()[1] < -100.0 && recent.max()[1] > -100.0);
        app.paused_at = Some(20.0);
        let id = egui::Id::new(("measurement-plot", 0_usize));
        let mut memory = PlotMemory::load(&ctx, id).unwrap();
        memory.set_bounds(PlotBounds::from_min_max([0.0, -1.0], [5.0, 1.0]));
        memory.store(&ctx, id);
        for _ in 0..3 {
            let older = frame(&mut app, &ctx);
            assert_eq!(older.min()[0], 0.0);
            assert_eq!(older.max()[0], 5.0);
            assert!(older.min()[1] < -1000000.0 && older.max()[1] > -1000000.0);
        }
        app.paused_at = None;
        let resumed = frame(&mut app, &ctx);
        assert_eq!(resumed.min()[0], 15.0);
        assert_eq!(resumed.max()[0], 20.0);
        assert!(resumed.min()[1] > -1000.0);
    }
}
