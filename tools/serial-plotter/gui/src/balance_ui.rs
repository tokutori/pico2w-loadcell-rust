use crate::{
    COLORS,
    balance::{self, Balance, Config},
    balance_settings,
    balance_trace::{Trace, trail_style},
    history::History,
};
use eframe::egui;
use egui_plot::{Line, Plot, PlotPoint, Points};
use std::path::Path;

pub struct Panel {
    pub config: Config,
    path: String,
    message: String,
    dirty: bool,
    calibration_channel: usize,
    known_mass: f64,
    pub trace: Trace,
    discontinuities: [u64; 4],
    auto_pending: bool,
    auto_since: Option<f64>,
    session_revision: u64,
    bar_scale_kg: f64,
}

impl Default for Panel {
    fn default() -> Self {
        Self {
            config: Config::default(),
            path: balance_settings::default_path()
                .to_string_lossy()
                .into_owned(),
            message:
                "Default frame: 600 x 2000 mm. Adjust support positions and calibrate each sensor."
                    .into(),
            dirty: false,
            calibration_channel: 0,
            known_mass: 1.0,
            trace: Trace::default(),
            discontinuities: [0; 4],
            auto_pending: false,
            auto_since: None,
            session_revision: 0,
            bar_scale_kg: 50.0,
        }
    }
}

impl Panel {
    pub fn end_session(&mut self) {
        self.auto_pending = false;
        self.auto_since = None;
        self.session_revision = 0;
    }
    pub fn begin_session(&mut self) {
        self.trace = Trace::default();
        self.auto_pending = self.config.auto_zero_on_connect;
        self.auto_since = None;
        self.message = if self.auto_pending {
            "PC auto zero: remove the object; keep the empty platform still.".into()
        } else {
            "PC auto zero off: using saved zero; capture again if needed.".into()
        };
    }

    pub fn zero_pending(&self) -> bool {
        self.auto_pending && self.config.auto_zero_on_connect
    }

    pub fn capture_zero(&mut self, history: &History, now: f64) -> Result<(), String> {
        self.config.zero_all(history, now)?;
        self.auto_pending = false;
        self.auto_since = None;
        self.trace = Trace::default();
        self.dirty = true;
        self.message =
            "PC zero captured. Place the object to measure. Save settings to retain this zero."
                .into();
        Ok(())
    }

    pub fn value(
        &self,
        index: usize,
        measurement: serial_plotter_core::Measurement,
        metric: serial_plotter_core::Metric,
    ) -> Option<f64> {
        if self.zero_pending() && metric != serial_plotter_core::Metric::Raw {
            return None;
        }
        self.config.value(index, measurement, metric)
    }

    /// Common USB/BLE controls; they change PC offsets and never send commands.
    pub fn tare_controls(
        &mut self,
        ui: &mut egui::Ui,
        history: &History,
        now: f64,
        connected: bool,
        demo: bool,
    ) {
        ui.horizontal_wrapped(|ui| {
            ui.strong("PC zero");
            if ui
                .add_enabled(
                    !demo,
                    egui::Checkbox::new(
                        &mut self.config.auto_zero_on_connect,
                        "Auto zero on connection",
                    ),
                )
                .changed()
            {
                self.dirty = true;
                if !self.config.auto_zero_on_connect {
                    self.auto_pending = false;
                    self.auto_since = None;
                    self.message = "PC auto zero off; existing zero retained.".into();
                } else {
                    self.message = "PC auto zero enabled for the next connection/session; use Zero PC to capture now.".into();
                }
            }
            for (i, cell) in self.config.cells.iter_mut().enumerate() {
                if ui
                    .add_enabled(
                        !demo,
                        egui::Checkbox::new(&mut cell.enabled, format!("LC{}", i + 1)),
                    )
                    .changed()
                {
                    self.auto_since = None;
                    self.dirty = true;
                }
            }
            if ui
                .add_enabled(
                    connected && !demo,
                    egui::Button::new("Zero PC (enabled sensors)"),
                )
                .clicked()
                && let Err(error) = self.capture_zero(history, now)
            {
                self.message = error;
            }
        });
        ui.label("Remove the object; leave the empty platform installed. Zero uses the recent raw average. Enable every supporting sensor.");
        if self.zero_pending() {
            ui.colored_label(
                egui::Color32::YELLOW,
                "PC auto zero pending: waiting for fresh data from all enabled sensors.",
            );
        }
        ui.label(&self.message);
        ui.horizontal_wrapped(|ui| {
            for (i, cell) in self
                .config
                .cells
                .iter()
                .enumerate()
                .filter(|(_, c)| c.enabled)
            {
                let kg = self
                    .config
                    .validate()
                    .ok()
                    .and_then(|()| balance::mean(history, &self.config, i, now).ok())
                    .and_then(|m| cell.zero_raw.map(|z| (m.raw - z) / cell.counts_per_kg));
                let text = if !connected || self.zero_pending() {
                    "--".into()
                } else {
                    kg.map(|kg| {
                        format!(
                            "{kg:.3} kg{}",
                            if cell.calibrated { "" } else { " (nominal)" }
                        )
                    })
                    .unwrap_or_else(|| "-- (zero/data needed)".into())
                };
                ui.colored_label(COLORS[i], format!("LC{}: {text}", i + 1));
            }
        });
    }

    fn check_gaps(&mut self, history: &History) {
        for (i, cell) in self.config.cells.iter().enumerate() {
            if cell.enabled && self.discontinuities[i] != history.discontinuities[i] {
                self.trace.break_line();
                self.auto_since = None;
            }
        }
        self.discontinuities = history.discontinuities;
    }

    pub fn observe(&mut self, history: &History, now: f64) {
        if self.session_revision != history.session_revision {
            self.session_revision = history.session_revision;
            self.begin_session();
        }
        self.check_gaps(history);
        if self.zero_pending() {
            match self
                .config
                .validate()
                .and_then(|()| self.config.means(history, now))
            {
                Ok(_) => {
                    let since = *self.auto_since.get_or_insert(now);
                    if now - since >= self.config.average_s
                        && let Err(error) = self.capture_zero(history, now)
                    {
                        self.message = error;
                    }
                }
                Err(error) => {
                    self.auto_since = None;
                    self.message = format!("PC auto zero waiting: {error}");
                }
            }
        }
        let value = if self.zero_pending() {
            Err("PC zero pending".into())
        } else {
            self.config.calculate(history, now)
        };
        self.trace.update(
            &self.config,
            history.line_count,
            history.last_time,
            value.as_ref().ok(),
        );
    }

    pub fn load_default(&mut self) {
        if Path::new(&self.path).exists() {
            self.load();
        }
    }

    fn load(&mut self) {
        match balance_settings::load(Path::new(&self.path)) {
            Ok(config) => {
                self.config = config;
                self.dirty = false;
                self.message =
                    "Settings loaded. Verify the platform-only zero before measuring.".into();
            }
            Err(error) => self.message = error,
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        history: &History,
        now: f64,
        connected: bool,
        demo: bool,
    ) {
        ui.heading("Center of gravity (horizontal)");
        ui.label("Origin: midpoint of front pair. +x = right, +y = rear. All positions in mm.");
        ui.label("Static, level platform; all supporting forces must pass through the enabled sensors. Height is not measured.");
        if demo {
            ui.colored_label(
                egui::Color32::YELLOW,
                "DEMO: simulated readings; not a physical measurement.",
            );
        }
        let result = if self.zero_pending() {
            Err("PC zero pending: keep the empty platform still".into())
        } else if connected || demo {
            self.config.calculate(history, now)
        } else {
            Err("Disconnected: no live CG. Connect to receive fresh readings.".into())
        };
        self.check_gaps(history);
        self.trace.update(
            &self.config,
            history.line_count,
            history.last_time,
            result.as_ref().ok(),
        );
        if !demo
            && self
                .trace
                .points
                .back()
                .is_some_and(|p| now - p.seconds <= self.config.trace_seconds.max(1.0))
        {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
        }
        match &result {
            Ok(cg) => {
                ui.heading(format!(
                    "x (right): {:+.1} mm     y (rear): {:+.1} mm     Total: {:.3} kg",
                    cg.x_mm, cg.y_mm, cg.total_kg
                ));
                if cg.nominal {
                    ui.colored_label(egui::Color32::YELLOW, "NOMINAL: one or more sensors still use an unverified span. Calibrate before trusting CG.");
                }
                ui.horizontal_wrapped(|ui| {
                    for (i, kg) in cg.loads.iter().enumerate() {
                        if let Some(kg) = kg {
                            ui.colored_label(
                                COLORS[i],
                                format!(
                                    "LC{}: {kg:.3} kg ({:.1}%)",
                                    i + 1,
                                    kg / cg.total_kg * 100.0
                                ),
                            );
                        }
                    }
                });
                if ui.button("Copy CG result (CSV)").clicked() {
                    let mut csv = format!(
                        "pc_receive_s,total_kg,x_right_mm,y_rear_mm,nominal,lc1_kg,lc2_kg,lc3_kg,lc4_kg\n{now:.3},{:.6},{:.3},{:.3},{}",
                        cg.total_kg, cg.x_mm, cg.y_mm, cg.nominal
                    );
                    for kg in cg.loads {
                        csv.push(',');
                        if let Some(kg) = kg {
                            csv.push_str(&format!("{kg:.6}"));
                        }
                    }
                    csv.push('\n');
                    ui.ctx().copy_text(csv);
                }
            }
            Err(error) => {
                ui.colored_label(egui::Color32::YELLOW, format!("CG unavailable: {error}"));
            }
        }
        let before = self.config.clone();
        ui.label(format!(
            "CG uses a {:.2} s mean of raw readings. Open Sensor settings / calibration to edit.",
            self.config.average_s
        ));
        ui.horizontal_wrapped(|ui| {
            ui.label("t-x / t-y window");
            ui.add(
                egui::DragValue::new(&mut self.config.trace_seconds)
                    .range(1.0..=300.0)
                    .suffix(" s"),
            );
            ui.label("XY trail: last 1 s; older = lighter and thinner");
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Load bar full height");
            ui.add(
                egui::DragValue::new(&mut self.bar_scale_kg)
                    .speed(0.5)
                    .range(0.01..=1e6)
                    .suffix(" kg"),
            );
            ui.label("ON/OFF: enabled / disabled | RX: fresh data | X: unavailable | *: nominal");
        });
        let readings = std::array::from_fn(|i| {
            balance::sensor_reading(
                history,
                &self.config,
                i,
                now,
                connected || demo,
                self.zero_pending(),
            )
        });
        let height = (ui.clip_rect().bottom() - ui.cursor().min.y - 24.0).clamp(380.0, 680.0);
        if ui.available_width() >= 780.0 {
            let gap = ui.spacing().item_spacing.x;
            let left = (ui.available_width() * 0.34).clamp(280.0, 420.0);
            let right = ui.available_width() - left - gap;
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(left, height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(left);
                        self.plot(ui, result.as_ref().ok(), now, height, &readings);
                    },
                );
                ui.allocate_ui_with_layout(
                    egui::vec2(right, height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(right);
                        self.time_graphs(ui, now, (height - ui.spacing().item_spacing.y) * 0.5);
                    },
                );
            });
        } else {
            self.plot(ui, result.as_ref().ok(), now, height, &readings);
            self.time_graphs(ui, now, 180.0);
        }
        self.dirty |= before != self.config;
    }

    pub fn show_settings(
        &mut self,
        ui: &mut egui::Ui,
        history: &History,
        now: f64,
        connected: bool,
        demo: bool,
    ) {
        ui.heading("Sensor settings / calibration");
        ui.label("Sensor positions, channel assignment, zero offsets and signed sensitivity are configured here.");
        let before = self.config.clone();
        ui.group(|ui| {
            ui.heading("1. Measurement point coordinates (mm)");
            ui.label("Upper = front; lower = rear. +x = right, +y = rear. Use the front-pair midpoint as your fixed reference origin.");
            ui.label("Coordinates belong to the measurement points; changing them does not reassign sensors.");
            if ui.button("Apply 600 x 2000 mm frame").clicked() {
                self.config.layout.points = balance::Layout::default().points;
                self.message = "600 x 2000 mm coordinates applied; sensor assignments and calibration retained.".into();
            }
            egui::Grid::new("measurement-points").striped(true).show(ui, |ui| {
                ui.strong("Measurement point"); ui.strong("x: right (mm)"); ui.strong("y: rear (mm)"); ui.end_row();
                for (role, name) in balance::POINT_NAMES.into_iter().enumerate() {
                    ui.label(format!("{}: {name}", balance::POINT_TAGS[role]));
                    for axis in 0..2 {
                        ui.add(egui::DragValue::new(&mut self.config.layout.points[role][axis]).speed(1.0).range(-1e6..=1e6));
                    }
                    ui.end_row();
                }
            });
        });
        ui.group(|ui| {
            ui.heading("2. Point to sensor assignment");
            ui.label("Choose which LC measures each point. Selecting an assigned LC swaps the two sensors; point coordinates stay fixed.");
            egui::Grid::new("point-sensors").striped(true).show(ui, |ui| {
                ui.strong("Measurement point"); ui.strong("Physical sensor"); ui.end_row();
                for (role, name) in balance::POINT_NAMES.into_iter().enumerate() {
                    ui.label(format!("{}: {name}", balance::POINT_TAGS[role]));
                    let mut selected = self.config.layout.channels[role];
                    egui::ComboBox::from_id_salt(("point-channel", role)).selected_text(format!("LC{}", selected + 1)).show_ui(ui, |ui| {
                        for i in 0..4 { ui.selectable_value(&mut selected, i, format!("LC{}", i + 1)); }
                    });
                    if selected != self.config.layout.channels[role] {
                        let previous = self.config.layout.channels[role];
                        self.message = match self.config.layout.assign(role, selected) {
                            Ok(()) => format!("Swapped LC{} and LC{}. Coordinates and per-sensor zero/span retained.", previous + 1, selected + 1),
                            Err(error) => error,
                        };
                    }
                    ui.end_row();
                }
            });
        });
        egui::CollapsingHeader::new("3. Sensor calibration / PC zero").default_open(true).show(ui, |ui| {
            ui.label("PC kg = (raw - PC zero) / signed counts per kg. Firmware delta and kg~ are not used for CG.");
            ui.label("Zero with the empty platform installed; remove only the object. Wait for stable readings over the averaging window before capture.");
            ui.add_enabled_ui(connected && !demo, |ui| {
                if ui.button("Capture platform-only zero (all enabled sensors)").clicked()
                    && let Err(error) = self.capture_zero(history, now) { self.message = error; }
            });
            let positions = self.config.layout.positions();
            let validation = self.config.validate();
            let means: [_; 4] = std::array::from_fn(|i| {
                validation.clone().and_then(|()| balance::mean(history, &self.config, i, now))
            });
            egui::ScrollArea::horizontal().id_salt("cg-calibration-table").show(ui, |ui| {
                egui::Grid::new("cg-cells").striped(true).show(ui, |ui| {
                    for label in ["Use", "Position (x, y) mm", "Mean raw / samples", "PC zero (raw)", "Signed counts/kg", "Span verified", "Capture"] { ui.strong(label); }
                    ui.end_row();
                    for (i, c) in self.config.cells.iter_mut().enumerate() {
                        ui.checkbox(&mut c.enabled, format!("LC{}", i + 1));
                        ui.label(format!("({:.1}, {:.1})", positions[i][0], positions[i][1]));
                        ui.label(match &means[i] { Ok(m) => format!("{:.1} / {}", m.raw, m.count), Err(_) => "--".into() })
                            .on_hover_text(means[i].as_ref().err().map(String::as_str).unwrap_or("Mean of the current healthy raw window"));
                        let mut zero = c.zero_raw.unwrap_or(0.0);
                        ui.vertical(|ui| {
                            if ui.add(egui::DragValue::new(&mut zero).speed(1.0)).changed() { c.zero_raw = Some(zero); }
                            let mut set = c.zero_raw.is_some();
                            if ui.checkbox(&mut set, "Zero set").changed() { c.zero_raw = set.then_some(zero); }
                        });
                        if ui.add(egui::DragValue::new(&mut c.counts_per_kg).speed(1.0)).changed() { c.calibrated = false; }
                        ui.checkbox(&mut c.calibrated, "Verified").on_hover_text("Check after entering a known calibrated signed sensitivity; nominal defaults are not verified.");
                        if ui.add_enabled(connected && !demo && means[i].is_ok(), egui::Button::new("Zero")).clicked() {
                            c.zero_raw = means[i].as_ref().ok().map(|m| m.raw);
                            self.auto_pending = false;
                            self.auto_since = None;
                            self.message = format!("LC{} zero captured", i + 1);
                        }
                        ui.end_row();
                    }
                });
            });
            ui.horizontal_wrapped(|ui| {
                egui::ComboBox::from_id_salt("cg-calibrate-channel").selected_text(format!("Calibrate LC{}", self.calibration_channel + 1)).show_ui(ui, |ui| {
                    for i in 0..4 { ui.selectable_value(&mut self.calibration_channel, i, format!("LC{}", i + 1)); }
                });
                ui.label("Known load on this sensor only");
                ui.add(egui::DragValue::new(&mut self.known_mass).speed(0.1).suffix(" kg"));
                if ui.add_enabled(connected && !demo, egui::Button::new("Capture span (auto polarity)")).clicked() {
                    self.message = match means[self.calibration_channel].clone().and_then(|m| self.config.calibrate(self.calibration_channel, m, self.known_mass)) {
                        Ok(()) => "Signed sensitivity captured. Repeat for each sensor, then zero the assembled empty platform.".into(), Err(e) => e,
                    };
                }
            });
            ui.label("Do not enter the whole object's mass as one sensor's load when a platform shares it across sensors. Calibrate sensors individually, then capture platform-only zero.");
            ui.label("Only disable a sensor that is NOT supporting the platform. Hiding a failed supporting sensor gives an incorrect CG.");
        });
        egui::CollapsingHeader::new("Averaging and validity limits").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (label, value) in [("Average window (s)", &mut self.config.average_s), ("Max data age (s)", &mut self.config.max_age_s), ("Max channel time skew (s)", &mut self.config.max_skew_s), ("Minimum total (kg)", &mut self.config.min_total_kg), ("Negative tolerance (kg)", &mut self.config.negative_tolerance_kg)] {
                    ui.label(label); ui.add(egui::DragValue::new(value).speed(0.01));
                }
                ui.label("Minimum samples / sensor");
                ui.add(egui::DragValue::new(&mut self.config.min_samples).range(1..=800));
            });
            ui.label("Freshness uses PC receive time. Measurements are asynchronous; use a static load. Small negative loads are preserved, not clamped.");
        });
        self.dirty |= before != self.config;
        if let Err(error) = self.config.validate() {
            ui.colored_label(egui::Color32::LIGHT_RED, error);
        }
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            ui.label("Settings JSON");
            ui.add(egui::TextEdit::singleline(&mut self.path).desired_width(450.0));
            if ui
                .add_enabled(!demo, egui::Button::new("Save settings"))
                .clicked()
            {
                self.message = match balance_settings::save(Path::new(&self.path), &self.config) {
                    Ok(()) => {
                        self.dirty = false;
                        "Settings saved.".into()
                    }
                    Err(e) => e,
                };
            }
            if ui
                .add_enabled(!demo, egui::Button::new("Load settings"))
                .clicked()
            {
                self.load();
            }
            if self.dirty {
                ui.colored_label(egui::Color32::YELLOW, "Unsaved changes");
            }
        });
        ui.label(&self.message);
        ui.label("Save explicitly before closing. The default settings file is loaded at startup; other JSON paths can be saved/loaded here.");
    }

    fn plot(
        &self,
        ui: &mut egui::Ui,
        result: Option<&Balance>,
        now: f64,
        height: f32,
        readings: &[balance::SensorReading; 4],
    ) {
        let response = Plot::new("cg-layout")
            .id(egui::Id::new("cg-layout"))
            .height(height)
            .data_aspect(1.0)
            .set_margin_fraction(egui::vec2(0.3, 0.35))
            .x_axis_label("Right +x (mm)")
            .y_axis_label("Forward -y (mm); rear is down")
            .include_x(0.0)
            .include_y(0.0)
            .show(ui, |plot| {
                let positions = self.config.layout.positions();
                let frame: Vec<_> = [0, 1, 3, 2, 0]
                    .into_iter()
                    .filter_map(|role| positions.get(self.config.layout.channels[role]))
                    .map(|&[x, y]| balance::plot_position(x, y))
                    .collect();
                if frame.len() == 5 && frame.iter().flatten().all(|v| v.is_finite()) {
                    plot.line(
                        Line::new("Support frame", frame)
                            .color(egui::Color32::GRAY)
                            .width(1.5_f32),
                    );
                }
                for segment in self.trace.segments(now, 1.0) {
                    for pair in segment.windows(2) {
                        let age = now - (pair[0].seconds + pair[1].seconds) * 0.5;
                        let (width, alpha) = trail_style(age);
                        plot.line(
                            Line::new(
                                "CG trail (last 1 s)",
                                vec![
                                    balance::plot_position(pair[0].x, pair[0].y),
                                    balance::plot_position(pair[1].x, pair[1].y),
                                ],
                            )
                            .color(egui::Color32::from_rgba_unmultiplied(255, 170, 180, alpha))
                            .width(width),
                        );
                    }
                }
                for (i, [x, y]) in self.config.layout.positions().into_iter().enumerate() {
                    if !x.is_finite() || !y.is_finite() {
                        continue;
                    }
                    let pos = balance::plot_position(x, y);
                    let color = if self.config.cells[i].enabled {
                        COLORS[i]
                    } else {
                        egui::Color32::GRAY
                    };
                    plot.points(
                        Points::new(format!("LC{}", i + 1), vec![pos])
                            .color(color)
                            .radius(6.0_f32),
                    );
                }
                plot.points(
                    Points::new("Front midpoint (origin)", vec![[0.0, 0.0]])
                        .color(egui::Color32::WHITE)
                        .radius(3.0_f32),
                );
                if let Some(cg) = result {
                    plot.points(
                        Points::new("CG", vec![balance::plot_position(cg.x_mm, cg.y_mm)])
                            .color(egui::Color32::from_rgb(235, 55, 85))
                            .radius(9.0_f32),
                    );
                }
            });
        // Screen-space bars retain one common kg scale during plot zoom/pan;
        // their baseline follows the measurement point, never the plot edge.
        let painter = ui.painter().with_clip_rect(*response.transform.frame());
        let full_height = response.transform.frame().height() * 0.18;
        let center_x = self.config.layout.points.iter().map(|p| p[0]).sum::<f64>() / 4.0;
        for (role, &i) in self.config.layout.channels.iter().enumerate() {
            let [x, y] = self.config.layout.points[role];
            if !x.is_finite() || !y.is_finite() {
                continue;
            }
            let point = response
                .transform
                .position_from_point(&PlotPoint::new(x, -y));
            let base = point + egui::vec2(if x >= center_x { 18.0 } else { -18.0 }, 0.0);
            let enabled = self.config.cells[i].enabled;
            let color = if enabled {
                COLORS[i]
            } else {
                egui::Color32::GRAY
            };
            let state = readings[i];
            let status = match state {
                balance::SensorReading::Unavailable(reason) => {
                    let cross_color = if enabled {
                        egui::Color32::LIGHT_RED
                    } else {
                        color
                    };
                    for sign in [-1.0, 1.0] {
                        painter.line_segment(
                            [
                                base + egui::vec2(-6.0, -6.0 * sign),
                                base + egui::vec2(6.0, 6.0 * sign),
                            ],
                            (2.5, cross_color),
                        );
                    }
                    format!("X: {reason}")
                }
                balance::SensorReading::Pending(reason) => {
                    painter.circle_stroke(base, 5.0, (1.5, color));
                    reason.into()
                }
                balance::SensorReading::Load { kg, nominal } => {
                    if enabled {
                        let negative = kg < 0.0;
                        let bar_color = if negative {
                            egui::Color32::LIGHT_RED
                        } else {
                            color
                        };
                        let top = base - egui::vec2(0.0, full_height);
                        painter.rect_stroke(
                            egui::Rect::from_two_pos(
                                top - egui::vec2(5.0, 0.0),
                                base + egui::vec2(5.0, 0.0),
                            ),
                            0.0,
                            (1.0, color.gamma_multiply(0.5)),
                            egui::StrokeKind::Inside,
                        );
                        // Negative values are shown explicitly, never converted to positive loads.
                        if !negative {
                            let h = (kg / self.bar_scale_kg).clamp(0.0, 1.0) as f32 * full_height;
                            painter.rect_filled(
                                egui::Rect::from_two_pos(
                                    base - egui::vec2(5.0, h),
                                    base + egui::vec2(5.0, 0.0),
                                ),
                                0.0,
                                bar_color,
                            );
                        } else {
                            painter.text(
                                base - egui::vec2(0.0, 12.0),
                                egui::Align2::CENTER_BOTTOM,
                                "!",
                                egui::FontId::proportional(16.0),
                                bar_color,
                            );
                        }
                        painter.line_segment(
                            [base - egui::vec2(8.0, 0.0), base + egui::vec2(8.0, 0.0)],
                            (1.5, color),
                        );
                        if kg > self.bar_scale_kg {
                            painter.text(
                                top - egui::vec2(0.0, 2.0),
                                egui::Align2::CENTER_BOTTOM,
                                "+",
                                egui::FontId::proportional(14.0),
                                color,
                            );
                        }
                    }
                    format!(
                        "RX: {kg:.2} kg{}{}",
                        if nominal { "*" } else { "" },
                        if kg < 0.0 { " !" } else { "" }
                    )
                }
            };
            let text_color = if enabled && matches!(state, balance::SensorReading::Unavailable(_)) {
                egui::Color32::LIGHT_RED
            } else {
                color
            };
            let label = painter.layout_no_wrap(
                format!(
                    "{}: LC{} {}\n{status}",
                    balance::POINT_TAGS[role],
                    i + 1,
                    if enabled { "ON" } else { "OFF" }
                ),
                egui::FontId::proportional(11.0),
                text_color,
            );
            let frame = response.transform.frame();
            let label_x = if x >= center_x {
                point.x + 8.0
            } else {
                point.x - 8.0 - label.size().x
            };
            let label_x = label_x.clamp(
                frame.left(),
                (frame.right() - label.size().x).max(frame.left()),
            );
            painter.galley(egui::pos2(label_x, point.y + 10.0), label, text_color);
        }
    }

    fn time_graphs(&self, ui: &mut egui::Ui, now: f64, height: f32) {
        let window = self.config.trace_seconds.clamp(1.0, 300.0);
        let end = now.max(1.0);
        let start = (end - window).max(0.0);
        let segments = self.trace.segments(now, window);
        for (axis, label, color) in [
            (0, "t-x: right position (mm)", COLORS[0]),
            (1, "t-y: rear position (mm)", COLORS[1]),
        ] {
            Plot::new(("cg-time", axis))
                .id(egui::Id::new(("cg-time", axis)))
                .height(height)
                .x_axis_label("PC receive time (s)")
                .y_axis_label(label)
                .allow_drag([false, true])
                .allow_zoom([false, true])
                .show(ui, |plot| {
                    plot.set_auto_bounds([false, true]);
                    plot.set_plot_bounds_x(start..=end);
                    for segment in &segments {
                        let points: Vec<_> = segment
                            .iter()
                            .map(|p| [p.seconds, if axis == 0 { p.x } else { p.y }])
                            .collect();
                        if points.len() == 1 {
                            plot.points(Points::new(label, points).color(color).radius(2.5_f32));
                        } else {
                            plot.line(Line::new(label, points).color(color).width(1.8_f32));
                        }
                    }
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_overlay_tracks_assigned_points_and_hides_disabled_or_unavailable_bars() {
        let mut panel = Panel::default();
        panel.config.layout.assign(0, 2).unwrap();
        panel.config.cells[1].enabled = false;
        let readings = [
            balance::SensorReading::Load {
                kg: 10.0,
                nominal: false,
            },
            balance::SensorReading::Load {
                kg: 25.0,
                nominal: false,
            },
            balance::SensorReading::Pending("RX: zero needed"),
            balance::SensorReading::Unavailable("timeout"),
        ];
        for width in [280.0, 420.0] {
            let ctx = egui::Context::default();
            for _ in 0..3 {
                let output = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 650.0),
                        )),
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default()
                            .show(ctx, |ui| panel.plot(ui, None, 0.0, 600.0, &readings));
                    },
                );
                let memory = egui_plot::PlotMemory::load(&ctx, egui::Id::new("cg-layout")).unwrap();
                let transform = memory.transform();
                let base = transform.position_from_point(&PlotPoint::new(300.0, -2000.0))
                    + egui::vec2(18.0, 0.0);
                let mut bars = Vec::new();
                let mut labels = Vec::new();
                for clipped in &output.shapes {
                    match &clipped.shape {
                        egui::Shape::Rect(r) if r.fill == COLORS[0] || r.fill == COLORS[1] => {
                            bars.push(r)
                        }
                        egui::Shape::Text(t) if t.galley.job.text.contains(": LC") => {
                            assert!(
                                clipped.clip_rect.contains_rect(egui::Rect::from_min_size(
                                    t.pos,
                                    t.galley.size()
                                ))
                            );
                            labels.push(t.galley.job.text.as_str());
                        }
                        _ => {}
                    }
                }
                assert_eq!(bars.len(), 1);
                assert!((bars[0].rect.bottom() - base.y).abs() < 0.1);
                assert!((bars[0].rect.center().x - base.x).abs() < 0.1);
                assert!(
                    (bars[0].rect.height() - transform.frame().height() * 0.18 * 10.0 / 50.0).abs()
                        < 0.1
                );
                assert!(labels.contains(&"UR: LC3 ON\nRX: zero needed"));
                assert!(labels.contains(&"UL: LC2 OFF\nRX: 25.00 kg"));
                assert!(labels.contains(&"LL: LC4 ON\nX: timeout"));
            }
        }
    }
}
