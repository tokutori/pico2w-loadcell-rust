use super::*;

impl Plotter {
    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.strong("Load cells");
            ui.separator();
            ui.add_enabled_ui(self.source.session().is_none(), |ui| {
                ui.selectable_value(&mut self.transport, TransportChoice::Usb, "USB");
                ui.selectable_value(&mut self.transport, TransportChoice::Ble, "BLE");
                if self.transport == TransportChoice::Usb {
                    ui.add(egui::TextEdit::singleline(&mut self.port).desired_width(85.0)).on_hover_text("Serial port");
                }
                ui.menu_button("Connection details", |ui| {
                    ui.set_min_width(260.0);
                    if self.transport == TransportChoice::Ble {
                        ui.label("BLE address (optional)");
                        ui.text_edit_singleline(&mut self.ble_address).on_hover_text("Leave empty when one Loadcells board is available. Discovery takes 5 seconds.");
                    } else {
                        egui::ComboBox::from_id_salt("ports").selected_text("Available ports").show_ui(ui, |ui| {
                            for port in &self.ports { ui.selectable_value(&mut self.port, port.clone(), port); }
                        });
                        if ui.button("Refresh ports").clicked() { self.refresh_ports(); }
                        ui.horizontal(|ui| { ui.label("Baud"); ui.add(egui::DragValue::new(&mut self.baud).range(1..=4_000_000)); });
                    }
                });
            });
            if self.source.session().is_some() {
                if ui.button("Disconnect").clicked() { self.disconnect(); }
            } else if ui.button("Connect").clicked() { self.connect(); }
            let (label, color) = match &self.source {
                Source::Offline if matches!(self.status, ConnectionMessage::Error(_)) => ("Connection error", egui::Color32::LIGHT_RED),
                Source::Offline => ("Disconnected", egui::Color32::GRAY),
                Source::Demo { .. } => ("DEMO", egui::Color32::YELLOW),
                Source::Live { phase: ConnectionPhase::Opening, .. } => ("Connecting...", egui::Color32::YELLOW),
                Source::Live { phase: ConnectionPhase::Ready, .. } => ("Connected", egui::Color32::LIGHT_GREEN),
                Source::Live { phase: ConnectionPhase::Stopping, .. } => ("Disconnecting...", egui::Color32::GRAY),
            };
            ui.label(egui::RichText::new(label).strong().color(color)).on_hover_text(self.status.text());
            if self.source.is_demo() && ui.button("Exit demo").clicked() { self.leave_demo(); }
        });
        if let ConnectionMessage::Error(message) = &self.status {
            ui.colored_label(egui::Color32::LIGHT_RED, message);
        }
    }

    fn graph_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            for channel in Channel::ALL {
                ui.checkbox(&mut self.visible[channel.index()], egui::RichText::new(channel.label()).color(COLORS[channel.index()]))
                    .on_hover_text("Show this channel; this does not change the supporting-sensor configuration.");
            }
            ui.separator();
            ui.label("Window");
            if ui.add(egui::DragValue::new(&mut self.window_seconds).range(5.0..=3600.0).suffix(" s")).changed() { self.reframe_time = true; }
            let mut follow = self.paused_at.is_none();
            if ui.checkbox(&mut follow, "Follow latest").changed() { self.paused_at = if follow { None } else { Some(self.end_time()) }; }
            ui.checkbox(&mut self.pc_graphs, "PC calibration").on_hover_text("ON: current PC zero/span. OFF: legacy firmware delta/kg values, absent in raw-only firmware.");
            ui.label("?").on_hover_text("Auto Y fits the visible data. Apply Y fixes the limits; turn off Auto Y to pan/zoom Y. Uncheck Follow latest to inspect older data. Reception continues.");
        });
    }

    fn diagnostics(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.toggle_value(&mut self.log_open, "Serial log").on_hover_text("Show or hide the resizable received-text log.");
            ui.menu_button("Actions", |ui| {
                if ui.button("Clear measurement history").clicked() { self.history.clear(); self.balance.clear_history(); ui.close(); }
                if ui.add_enabled(self.source.session().is_none(), egui::Button::new("Sample log")).clicked() { self.load_demo(); ui.close(); }
                if ui.add_enabled(self.source.session().is_none(), egui::Button::new("CG sample")).clicked() { self.load_cg_demo(); ui.close(); }
                ui.separator();
                ui.label(format!("Legacy firmware tare: {:?}", self.history.tare));
                for (i, required) in self.history.tare_failures.iter().enumerate() {
                    if let Some(required) = required { ui.label(format!("LC{} tare failed ({required} samples required)", i + 1)); }
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let drops = self.history.dropped.iter().copied().fold(0_u64, u64::saturating_add);
                let text = format!("{} lines   |   {} errors   |   {drops} drops", self.history.line_count, self.history.errors);
                let color = if self.history.errors > 0 || drops > 0 { egui::Color32::YELLOW } else { ui.visuals().weak_text_color() };
                ui.label(egui::RichText::new(text).small().color(color)).on_hover_text("Retained input diagnostics: received lines, parser/framing errors, firmware queue drops. These counters do not indicate physical load accuracy.");
            });
        });
    }

    fn log(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("Serial monitor")
                .on_hover_text("Raw received text, latest 2,000 lines.");
            ui.checkbox(&mut self.follow_log, "Auto-scroll");
            if ui.button("Copy log").clicked() {
                ui.ctx().copy_text(self.history.monitor_text());
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
                        let text = self
                            .history
                            .lines
                            .get(i)
                            .cloned()
                            .unwrap_or_else(|| self.history.pending());
                        ui.add(
                            egui::Label::new(egui::RichText::new(text).monospace())
                                .extend()
                                .selectable(true),
                        );
                    }
                },
            );
    }

    pub(super) fn graphs(&mut self, ui: &mut egui::Ui) {
        let height = ((ui.available_height() - 114.0) / 3.0).max(65.0);
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

impl Plotter {
    /// All panels are rendered here so complete-window geometry is testable.
    pub(super) fn show_ui(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("connection-toolbar").show(ctx, |ui| self.controls(ui));
        egui::TopBottomPanel::bottom("diagnostics").show(ctx, |ui| self.diagnostics(ui));
        if self.log_open {
            egui::TopBottomPanel::bottom("monitor")
                .resizable(true)
                .default_height(150.0)
                .min_height(70.0)
                .max_height((ctx.content_rect().height() * 0.20).max(70.0))
                .show(ctx, |ui| self.log(ui));
        }
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
            ui.separator();
            let now = self.measurement_time();
            match self.view {
                View::Balance => {
                    self.balance.show(
                        ui,
                        &self.history,
                        now,
                        self.source.ready(),
                        self.source.is_demo(),
                    );
                }
                View::Graphs => {
                    let _ = self.balance.summary(
                        ui,
                        &self.history,
                        now,
                        self.source.ready(),
                        self.source.is_demo(),
                    );
                    self.graph_controls(ui);
                    self.graphs(ui);
                }
                View::Settings => {
                    egui::ScrollArea::vertical()
                        .id_salt("settings-page")
                        .show(ui, |ui| {
                            self.balance.show_settings(
                                ui,
                                &self.history,
                                now,
                                self.source.ready(),
                                self.source.is_demo(),
                            );
                        });
                }
            }
        });
    }
}

impl eframe::App for Plotter {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain();
        if self.source.session().is_some()
            || (!self.source.is_demo() && self.sample_counts().iter().any(|n| *n > 0))
        {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        self.show_ui(ctx);
    }
}
