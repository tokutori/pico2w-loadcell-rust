use super::*;

impl Plotter {
    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.heading("Load cell monitor");
            ui.separator();
            ui.add_enabled_ui(self.source.session().is_none(), |ui| {
                ui.selectable_value(&mut self.transport, TransportChoice::Usb, "USB serial");
                ui.selectable_value(&mut self.transport, TransportChoice::Ble, "BLE");
                if self.transport == TransportChoice::Ble {
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
                .add_enabled(self.source.session().is_some(), egui::Button::new("Disconnect"))
                .clicked()
            { self.disconnect(); }
        });
        ui.label(&self.status);
        ui.horizontal_wrapped(|ui| {
            ui.strong("Valid samples / last 1 s (PC received):");
            for (i, count) in self.sample_counts().into_iter().enumerate() {
                ui.colored_label(COLORS[i], format!("LC{}: {count}", i + 1));
            }
        });
        let now = self.measurement_time();
        self.balance.tare_controls(
            ui,
            &self.history,
            now,
            self.source.ready(),
            self.source.is_demo(),
        );
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
                self.balance.clear_history();
            }
            if ui
                .add_enabled(
                    self.source.session().is_none(),
                    egui::Button::new("Sample log"),
                )
                .clicked()
            {
                self.load_demo();
            }
            if ui
                .add_enabled(
                    self.source.session().is_none(),
                    egui::Button::new("CG sample"),
                )
                .clicked()
            {
                self.load_cg_demo();
            }
            if self.source.is_demo() && ui.button("Exit demo").clicked() {
                self.leave_demo();
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

    pub(super) fn graphs(&mut self, ui: &mut egui::Ui) {
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
        if self.source.session().is_some()
            || (!self.source.is_demo() && self.sample_counts().iter().any(|n| *n > 0))
        {
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
                    if self.source.is_demo() {
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
                                self.source.ready(),
                                self.source.is_demo(),
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
                                self.source.ready(),
                                self.source.is_demo(),
                            );
                        });
                }
            }
        });
    }
}
