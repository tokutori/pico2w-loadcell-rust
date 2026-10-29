#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod history;
mod transport;

use eframe::egui;
use egui_plot::{Legend, Line, Plot, Points};
use history::History;
use serial_plotter_core::{Channel, Fault, Measurement, Metric};
use std::time::{Duration, Instant};
use transport::{Event, Session};

const COLORS: [egui::Color32; 4] = [
    egui::Color32::from_rgb(60, 190, 230),
    egui::Color32::from_rgb(250, 185, 70),
    egui::Color32::from_rgb(140, 215, 115),
    egui::Color32::from_rgb(220, 125, 215),
];

struct Plotter {
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
    follow_log: bool,
    demo: bool,
}

impl Plotter {
    fn new(port: String, demo: bool) -> Self {
        let mut app = Self {
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
        if self.port.trim().is_empty() || self.baud == 0 {
            self.status = "Enter a port and a positive baud rate.".into();
            return;
        }
        let started = Instant::now();
        match Session::start(self.port.trim().to_owned(), self.baud) {
            Ok(session) => {
                self.session = Some(session);
                self.started = started;
                self.history = History::default();
                self.paused_at = None;
                self.demo = false;
                self.status = format!("Opening {}...", self.port);
            }
            Err(error) => self.status = format!("Cannot start serial reader: {error}"),
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
                    self.status = format!(
                        "Connected to {} | DTR on | firmware performs tare on connection",
                        self.port
                    )
                }
                Ok(Event::Bytes { at, bytes }) => self
                    .history
                    .ingest(at.duration_since(self.started).as_secs_f64(), &bytes),
                Err(_) => break,
            }
        }
        if session.is_finished() {
            // A finished reader cannot enqueue any more data. Drain its tail.
            for event in session.events.try_iter() {
                if let Event::Bytes { at, bytes } = event {
                    self.history
                        .ingest(at.duration_since(self.started).as_secs_f64(), &bytes);
                }
            }
            if let Some(session) = self.session.take() {
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

    fn load_demo(&mut self) {
        self.history = History::default();
        self.demo = true;
        self.paused_at = None;
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

    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.heading("Load cell monitor");
            ui.separator();
            ui.add_enabled_ui(self.session.is_none(), |ui| {
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
        ui.label("Before connecting: unload LC1 and keep it unloaded until 'Tare complete.' (may take about 18 s)");
        ui.horizontal_wrapped(|ui| {
            ui.label("Plot channels:");
            for channel in Channel::ALL {
                ui.checkbox(
                    &mut self.visible[channel.index()],
                    egui::RichText::new(channel.label()).color(COLORS[channel.index()]),
                );
            }
            ui.separator();
            ui.label("Window");
            ui.add(
                egui::DragValue::new(&mut self.window_seconds)
                    .range(5.0..=3600.0)
                    .suffix(" s"),
            );
            if ui
                .button(if self.paused_at.is_some() {
                    "Resume graphs"
                } else {
                    "Pause graphs"
                })
                .clicked()
            {
                self.paused_at = match self.paused_at {
                    Some(_) => None,
                    None => Some(self.end_time()),
                };
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
            ui.label(format!(
                "{} lines | {} parse/framing errors",
                self.history.line_count, self.history.errors
            ));
        });
        if self.paused_at.is_some() {
            ui.label("Graphs paused; serial reception and monitor continue. History is bounded to 10,000 entries/channel.");
        }
        ui.horizontal_wrapped(|ui| {
            for channel in Channel::ALL {
                let i = channel.index();
                if !self.visible[i] {
                    continue;
                }
                let text = match self.history.latest[i].map(|reading| reading.measurement) {
                    Some(Measurement::Sample { raw, delta, kg }) => {
                        format!("{}  raw={raw}  delta={delta}  kg~={kg:.3}", channel.label())
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
                    None => format!("{}  waiting for data", channel.label()),
                };
                ui.colored_label(COLORS[i], text);
            }
        });
    }

    fn graphs(&self, ui: &mut egui::Ui) {
        let height = ((ui.available_height() - 12.0) / 3.0).max(100.0);
        let end = self.end_time().max(1.0);
        let start = (end - self.window_seconds).max(0.0);
        let end = end.max(start + 1.0);
        for metric in Metric::ALL {
            let plot = Plot::new(metric.label())
                .height(height)
                .legend(Legend::default())
                .x_axis_label("PC receive time (s)")
                .y_axis_label(metric.label());
            let plot = if self.paused_at.is_none() {
                plot.reset()
            } else {
                plot
            };
            plot.show(ui, |plot| {
                if self.paused_at.is_none() {
                    plot.set_plot_bounds_x(start..=end);
                }
                for channel in Channel::ALL {
                    let i = channel.index();
                    if !self.visible[i] {
                        continue;
                    }
                    for points in self.history.segments(channel, metric, start, end) {
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
        }
    }
}

impl eframe::App for Plotter {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain();
        if self.session.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        egui::TopBottomPanel::top("controls").show(ctx, |ui| self.controls(ui));
        egui::TopBottomPanel::bottom("monitor")
            .resizable(true)
            .default_height(220.0)
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
        egui::CentralPanel::default().show(ctx, |ui| self.graphs(ui));
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
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--demo" => demo = true,
            "--port" => {
                let Some(value) = args.next() else {
                    eprintln!("--port requires a value");
                    std::process::exit(2);
                };
                port = value;
            }
            "--help" | "-h" => {
                println!(
                    "serial-plotter [--port COM5] [--demo]\nPorts are opened only when Connect is clicked."
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
            .with_inner_size([1200.0, 900.0])
            .with_min_inner_size([850.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Load Cell Serial Plotter",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            Ok(Box::new(Plotter::new(port, demo)))
        }),
    )
}
