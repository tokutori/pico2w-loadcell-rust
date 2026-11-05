use super::*;
use egui_plot::{PlotBounds, PlotMemory};

fn window_frame(app: &mut Plotter, ctx: &egui::Context, size: egui::Vec2) -> egui::FullOutput {
    ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..Default::default()
        },
        |ctx| app.show_ui(ctx),
    )
}

fn visible_text(output: &egui::FullOutput) -> Vec<(String, egui::Rect)> {
    fn collect(shape: &egui::Shape, clip: egui::Rect, out: &mut Vec<(String, egui::Rect)>) {
        match shape {
            egui::Shape::Text(t) => {
                let rect = t.galley.rect.translate(t.pos.to_vec2());
                if clip.intersects(rect) {
                    out.push((t.galley.job.text.clone(), rect));
                }
            }
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    collect(shape, clip, out);
                }
            }
            _ => {}
        }
    }
    let mut text = Vec::new();
    for shape in &output.shapes {
        collect(&shape.shape, shape.clip_rect, &mut text);
    }
    text
}

#[test]
fn full_window_plots_fit_with_and_without_log() {
    for size in [egui::vec2(850.0, 700.0), egui::vec2(1440.0, 1000.0)] {
        for log_open in [false, true] {
            for view in [View::Balance, View::Graphs] {
                let ctx = egui::Context::default();
                let mut app = Plotter::new(String::new(), false);
                app.load_cg_demo();
                app.balance.config.layout = balance::Layout::default();
                app.log_open = log_open;
                app.view = view;
                for _ in 0..3 {
                    let _ = window_frame(&mut app, &ctx, size);
                }
                let output = window_frame(&mut app, &ctx, size);
                let text = visible_text(&output);
                let bottom = text
                    .iter()
                    .find(|(t, _)| {
                        t == if log_open {
                            "Serial monitor"
                        } else {
                            "Serial log"
                        }
                    })
                    .unwrap()
                    .1
                    .top()
                    - 4.0;
                let ids = match view {
                    View::Balance => vec![
                        egui::Id::new("cg-layout"),
                        egui::Id::new(("cg-time", 0_usize)),
                        egui::Id::new(("cg-time", 1_usize)),
                    ],
                    View::Graphs => (0..3_usize)
                        .map(|i| egui::Id::new(("measurement-plot", i)))
                        .collect(),
                    View::Settings => unreachable!(),
                };
                for id in ids {
                    let memory = PlotMemory::load(&ctx, id).unwrap();
                    let rect = *memory.transform().frame();
                    assert!(
                        rect.bottom() <= bottom
                            && rect.right() <= size.x - 4.0
                            && rect.top() > 50.0
                            && rect.height() > 30.0,
                        "{size:?} log={log_open} view={view:?}: plot={rect:?}, bottom={bottom}"
                    );
                }
                let axis_labels: Vec<_> = text
                    .iter()
                    .filter(|(t, _)| t == "PC receive time (s)" || t == "Right +x (mm)")
                    .collect();
                assert_eq!(
                    axis_labels.len(),
                    3,
                    "Every plot must retain its x-axis label"
                );
                for (_, rect) in axis_labels {
                    assert!(
                        rect.bottom() <= bottom,
                        "{size:?} log={log_open} view={view:?}: axis label overlaps log/footer: {rect:?}, bottom={bottom}"
                    );
                }
                assert!(
                    text.iter()
                        .any(|(t, r)| t == "100.00000" && r.height() >= 24.0)
                );
            }
        }
    }
}

#[test]
fn offline_window_has_one_disconnect_badge_and_no_stale_load() {
    let ctx = egui::Context::default();
    let mut app = Plotter::new(String::new(), false);
    app.load_cg_demo();
    app.source = Source::Offline;
    let size = egui::vec2(850.0, 700.0);
    for _ in 0..3 {
        let _ = window_frame(&mut app, &ctx, size);
    }
    let text = visible_text(&window_frame(&mut app, &ctx, size));
    assert_eq!(
        text.iter()
            .filter(|(t, _)| t.contains("Disconnected"))
            .count(),
        1
    );
    assert!(
        !text
            .iter()
            .any(|(t, _)| t == "100.00000" || t.contains("CG unavailable"))
    );
    assert_eq!(text.iter().filter(|(t, _)| t == "-- kgf").count(), 4);
}

#[test]
fn live_fault_and_zeroing_remain_visible_without_repeating_global_state() {
    let ctx = egui::Context::default();
    let mut app = Plotter::new(String::new(), false);
    app.load_cg_demo();
    app.source = Source::Live {
        session: Session::simulated(),
        phase: ConnectionPhase::Ready,
    };
    app.started = Instant::now() - Duration::from_secs_f64(app.history.last_time);
    app.history
        .ingest(app.history.last_time, b"LC3 status=timeout\n");
    let size = egui::vec2(850.0, 700.0);
    for _ in 0..3 {
        let _ = window_frame(&mut app, &ctx, size);
    }
    let text = visible_text(&window_frame(&mut app, &ctx, size));
    assert_eq!(text.iter().filter(|(t, _)| t == "LC3: timeout").count(), 1);
    assert_eq!(text.iter().filter(|(t, _)| t == "Connected").count(), 1);
    assert!(text.iter().any(|(t, _)| t == "10.00000 kgf"));
    assert!(text.iter().any(|(t, _)| t == "-- kgf"));
    app.balance.config.auto_zero_on_connect = true;
    app.balance.begin_session();
    let text = visible_text(&window_frame(&mut app, &ctx, size));
    assert_eq!(
        text.iter().filter(|(t, _)| t.contains("Zeroing:")).count(),
        1
    );
    assert!(!text.iter().any(|(t, _)| t == "100.00000"));
}

#[test]
fn zero_button_clears_old_cg_before_redrawing_trace() {
    let ctx = egui::Context::default();
    let mut app = Plotter::new(String::new(), false);
    app.load_cg_demo();
    app.source = Source::Live {
        session: Session::simulated(),
        phase: ConnectionPhase::Ready,
    };
    app.started = Instant::now() - Duration::from_secs_f64(app.history.last_time);
    let size = egui::vec2(850.0, 700.0);
    for _ in 0..3 {
        let _ = window_frame(&mut app, &ctx, size);
    }
    assert!(!app.balance.trace.points.is_empty());
    let text = visible_text(&window_frame(&mut app, &ctx, size));
    let pos = text
        .iter()
        .find(|(t, _)| t == "Zero load")
        .unwrap()
        .1
        .center();
    for pressed in [true, false] {
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                events: vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..Default::default()
            },
            |ctx| app.show_ui(ctx),
        );
    }
    assert!(app.balance.trace.points.is_empty());
    let total = app
        .balance
        .config
        .total_load(&app.history, app.measurement_time())
        .unwrap();
    assert!(total.kgf.abs() < 1e-8);
}

#[test]
fn opening_and_stopping_are_not_ready_and_late_ready_cannot_cancel_stop() {
    let mut app = Plotter::new(String::new(), false);
    app.source = Source::Live {
        session: Session::simulated(),
        phase: ConnectionPhase::Opening,
    };
    assert!(!app.source.ready());
    app.handle_event(Event::Ready(transport::Link::Ble("fixture".into())));
    assert!(app.source.ready());
    app.disconnect();
    assert!(!app.source.ready());
    app.handle_event(Event::Ready(transport::Link::Usb("fixture".into())));
    assert!(!app.source.ready());
    assert_eq!(app.status.text(), "Disconnecting...");
}

#[test]
fn queued_old_measurements_cannot_capture_a_live_auto_zero() {
    let mut app = Plotter::new(String::new(), false);
    app.source = Source::Live {
        session: Session::simulated(),
        phase: ConnectionPhase::Ready,
    };
    app.started = Instant::now() - Duration::from_secs(10);
    app.balance.begin_session();
    for n in 0..9 {
        app.handle_event(Event::Bytes {
            at: app.started + Duration::from_millis(1000 + n * 200),
            bytes: b"LC1 raw=-100000\nLC2 raw=-100000\nLC3 raw=-100000\nLC4 raw=-100000\n".to_vec(),
        });
    }
    assert!(app.balance.zero_pending());
    assert!(
        app.balance
            .config
            .cells
            .iter()
            .all(|c| c.zero_raw.is_none())
    );
}

#[test]
fn both_demos_restore_real_configuration_and_discard_simulated_readings() {
    let mut app = Plotter::new(String::new(), false);
    app.balance.config.layout.points[0][0] = 456.0;
    let real = app.balance.config.clone();
    app.load_demo();
    app.balance.config.cells[0].enabled = false;
    app.load_cg_demo();
    app.load_demo();
    app.leave_demo();
    assert_eq!(app.balance.config, real);
    assert!(matches!(app.source, Source::Offline));
    assert_eq!(app.history.line_count, 0);
    assert!(app.balance.trace.points.is_empty());
}

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
    assert!(matches!(&app.source, Source::Demo { saved } if saved.config == real));
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
