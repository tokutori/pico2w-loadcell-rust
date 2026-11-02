use crate::{balance::*, history::History};

#[test]
fn point_readings_expire_disconnect_and_recover_without_hiding_other_sensors() {
    let config = configured();
    let mut history = readings([1, 2, 3, 4]);
    let load = |kg| SensorReading::Load { kg, nominal: false };
    assert_eq!(
        sensor_reading(&history, &config, 0, 1.2, true, false),
        load(1.0)
    );
    assert_eq!(
        sensor_reading(&history, &config, 0, 1.2, false, false),
        SensorReading::Unavailable("disconnected")
    );
    assert_eq!(
        sensor_reading(&history, &config, 0, 1.8, true, false),
        SensorReading::Unavailable("stale")
    );
    history.ingest(1.21, b"LC1 status=timeout\n");
    assert!(config.calculate(&history, 1.21).is_err());
    assert_eq!(
        sensor_reading(&history, &config, 0, 1.21, true, false),
        SensorReading::Unavailable("timeout")
    );
    assert_eq!(
        sensor_reading(&history, &config, 1, 1.21, true, false),
        load(2.0)
    );
    history.ingest(1.22, b"LC1 raw=-103000\n");
    assert_eq!(
        sensor_reading(&history, &config, 0, 1.22, true, false),
        SensorReading::Pending("RX: averaging")
    );
    history.ingest(1.23, b"LC1 raw=-103000\nLC1 raw=-103000\n");
    assert_eq!(
        sensor_reading(&history, &config, 0, 1.23, true, false),
        load(3.0)
    );
    history.transport_gap();
    assert_eq!(
        sensor_reading(&history, &config, 0, 1.23, true, false),
        SensorReading::Unavailable("data gap")
    );
}

#[test]
fn point_readings_distinguish_zero_pending_from_connection_and_keep_negative_loads() {
    let mut config = configured();
    let history = readings([-1, 0, 3, 4]);
    config.cells[0].enabled = false;
    config.cells[0].calibrated = false;
    assert_eq!(
        sensor_reading(&history, &config, 0, 1.2, true, false),
        SensorReading::Load {
            kg: -1.0,
            nominal: true
        }
    );
    assert_eq!(
        sensor_reading(&history, &config, 1, 1.2, true, false),
        SensorReading::Load {
            kg: 0.0,
            nominal: false
        }
    );
    assert_eq!(
        sensor_reading(&history, &config, 1, 1.2, true, true),
        SensorReading::Pending("RX: zero needed")
    );
    config.cells[1].zero_raw = None;
    assert_eq!(
        sensor_reading(&history, &config, 1, 1.2, true, false),
        SensorReading::Pending("RX: zero needed")
    );
    assert_eq!(
        sensor_reading(&History::default(), &config, 1, 1.2, true, false),
        SensorReading::Unavailable("no data")
    );
}

fn raw_frame(history: &mut History, at: f64, raw: [i32; 4]) {
    for (i, raw) in raw.into_iter().enumerate() {
        history.ingest(at, format!("LC{} raw={raw}\n", i + 1).as_bytes());
    }
}

#[test]
fn point_assignments_swap_without_moving_coordinates_or_sensor_calibration() {
    let mut config = configured();
    config.layout = Layout::default();
    let points = config.layout.points;
    let cells = config.cells.clone();
    let history = readings([1, 2, 3, 4]);
    close(config.calculate(&history, 1.2).unwrap().y_mm, 1400.0);
    config.layout.assign(0, 2).unwrap(); // Upper right: LC1 -> LC3, swaps lower right to LC1.
    assert_eq!(config.layout.channels, [2, 1, 0, 3]);
    assert_eq!(config.layout.points, points);
    assert_eq!(config.cells, cells);
    assert_eq!(config.layout.positions()[0], [300.0, 2000.0]);
    close(config.calculate(&history, 1.2).unwrap().y_mm, 1000.0);
    close(config.calculate(&history, 1.2).unwrap().total_kg, 10.0);
    for point in 0..4 {
        for channel in 0..4 {
            config.layout.assign(point, channel).unwrap();
            assert_eq!(config.layout.channels[point], channel);
            config.validate().unwrap();
            assert_eq!(config.layout.points, points);
            assert_eq!(config.cells, cells);
        }
    }
    let old = config.clone();
    assert!(config.layout.assign(4, 0).is_err());
    assert!(config.layout.assign(0, 4).is_err());
    assert_eq!(config, old);
    config.layout.points[0] = [275.0, 35.0];
    assert_eq!(config.layout.channels, old.layout.channels);
    assert_eq!(config.cells, old.cells);
    assert_eq!(
        config.layout.positions()[config.layout.channels[0]],
        [275.0, 35.0]
    );
}

#[test]
fn legacy_dimensions_load_as_points_and_new_settings_have_one_coordinate_source() {
    let legacy = r#"{"x1":100,"x2":100,"y":200,"dx":20,"dy":10,"channels":[2,3,0,1]}"#;
    let layout: Layout = serde_json::from_str(legacy).unwrap();
    assert_eq!(
        layout.points,
        [[100.0, 0.0], [-100.0, 0.0], [120.0, 210.0], [-80.0, 190.0]]
    );
    assert_eq!(layout.channels, [2, 3, 0, 1]);
    let json = serde_json::to_value(&layout).unwrap();
    assert!(json.get("x1").is_none());
    assert_eq!(
        serde_json::from_value::<Layout>(json.clone()).unwrap(),
        layout
    );
    let mut ambiguous = json;
    ambiguous["x1"] = serde_json::json!(300.0);
    assert!(serde_json::from_value::<Layout>(ambiguous).is_err());
    let mut invalid = configured();
    invalid.layout.points[0][0] = f64::NAN;
    assert!(invalid.validate().is_err());
}

#[test]
fn pc_auto_zero_runs_once_and_restarts_on_device_session_banner() {
    use serial_plotter_core::{Measurement, Metric};
    let mut panel = crate::balance_ui::Panel::default();
    panel.config = configured();
    panel.begin_session();
    assert!(panel.zero_pending());
    assert_eq!(
        panel.value(0, Measurement::Raw { raw: -123000 }, Metric::Kg),
        None
    );
    let mut history = History::default();
    let empty = [-123000, -98000, -97000, -96000];
    for i in 0..=12 {
        let t = i as f64 * 0.1;
        raw_frame(&mut history, t, empty);
        panel.observe(&history, t);
    }
    assert!(!panel.zero_pending());
    assert_eq!(
        panel.config.cells.each_ref().map(|c| c.zero_raw),
        empty.map(|v| Some(v as f64))
    );
    let zeroed = panel.config.clone();
    for i in 13..=25 {
        let t = i as f64 * 0.1;
        raw_frame(&mut history, t, [-124000, -96000, -94000, -92000]);
        panel.observe(&history, t);
    }
    assert_eq!(panel.config, zeroed); // Actual load must never be auto-zeroed repeatedly.
    let cg = panel.config.calculate(&history, 2.5).unwrap();
    close(cg.total_kg, 10.0);
    close(cg.x_mm, -20.0);
    for i in 0..4 {
        close(
            panel
                .config
                .value(i, history.latest[i].unwrap().measurement, Metric::Kg)
                .unwrap(),
            cg.loads[i].unwrap(),
        );
    }
    history.ingest(
        2.6,
        b"Session started. Raw output; zero and calibration are PC-side.\n",
    );
    panel.observe(&history, 2.6);
    assert!(panel.zero_pending());
    assert!(panel.trace.points.is_empty());
    assert!(panel.capture_zero(&history, 2.6).is_err()); // No reuse of previous session samples.
}

#[test]
fn manual_pc_zero_preserves_all_offsets_on_failure_and_respects_disabled_auto() {
    let mut panel = crate::balance_ui::Panel::default();
    panel.config = configured();
    panel.config.auto_zero_on_connect = false;
    panel.begin_session();
    let mut history = History::default();
    for i in 0..=10 {
        raw_frame(
            &mut history,
            i as f64 * 0.1,
            [-123000, -98000, -97000, -96000],
        );
    }
    let old = panel.config.clone();
    panel.observe(&history, 1.0);
    assert_eq!(panel.config, old);
    assert!(!panel.zero_pending());
    history.ingest(1.0, b"LC4 status=timeout\n");
    assert!(panel.capture_zero(&history, 1.0).is_err());
    assert_eq!(panel.config, old);
    panel.config.cells[3].enabled = false;
    panel.capture_zero(&history, 1.0).unwrap();
    assert_eq!(panel.config.cells[0].zero_raw, Some(-123000.0));
    assert_eq!(panel.config.cells[3].zero_raw, old.cells[3].zero_raw);
    assert!(panel.trace.points.is_empty());
    let after = panel.config.clone();
    assert!(panel.capture_zero(&history, 5.0).is_err());
    assert_eq!(panel.config, after);
}

#[test]
fn pc_auto_zero_waits_for_all_enabled_sensors_and_fresh_window_after_fault() {
    let mut panel = crate::balance_ui::Panel::default();
    panel.config = configured();
    panel.begin_session();
    let old = panel.config.clone();
    let mut history = History::default();
    for i in 0..=10 {
        let t = i as f64 * 0.1;
        raw_frame(&mut history, t, [-123000, -98000, -97000, -96000]);
        history.ingest(t, b"LC4 status=protocol\n");
        panel.observe(&history, t);
    }
    assert!(panel.zero_pending());
    assert_eq!(panel.config, old);
    for i in 11..=13 {
        let t = i as f64 * 0.1;
        raw_frame(&mut history, t, [-123000, -98000, -97000, -96000]);
        panel.observe(&history, t);
    }
    assert!(panel.zero_pending()); // Not enough elapsed fresh averaging time.
    for i in 14..=22 {
        let t = i as f64 * 0.1;
        raw_frame(&mut history, t, [-123000, -98000, -97000, -96000]);
        panel.observe(&history, t);
    }
    assert!(!panel.zero_pending());
}

#[test]
fn old_settings_gain_auto_zero_default_and_new_preference_roundtrips() {
    let mut json = serde_json::to_value(configured()).unwrap();
    json.as_object_mut().unwrap().remove("auto_zero_on_connect");
    let mut config: Config = serde_json::from_value(json).unwrap();
    assert!(config.auto_zero_on_connect);
    config.auto_zero_on_connect = false;
    assert_eq!(
        serde_json::from_str::<Config>(&serde_json::to_string(&config).unwrap()).unwrap(),
        config
    );
}

fn configured() -> Config {
    let mut config = Config {
        layout: Layout {
            points: [[100.0, 0.0], [-100.0, 0.0], [100.0, 200.0], [-100.0, 200.0]],
            channels: [0, 1, 2, 3],
        },
        ..Config::default()
    };
    for (i, c) in config.cells.iter_mut().enumerate() {
        c.zero_raw = Some(-100_000.0);
        c.counts_per_kg = if i == 0 { -1000.0 } else { 1000.0 };
        c.calibrated = true;
    }
    config
}

fn readings(weights: [i32; 4]) -> History {
    let mut history = History::default();
    for t in [1.0, 1.1, 1.2] {
        for (i, w) in weights.iter().enumerate() {
            let raw = -100_000 + w * if i == 0 { -1000 } else { 1000 };
            // Deliberately unrelated firmware kg/delta; PC must use raw only.
            let line = if i == 2 {
                format!("LC{} raw={raw} status=no_tare\n", i + 1)
            } else {
                format!("LC{} raw={raw} delta=123 kg~=999\n", i + 1)
            };
            history.ingest(t, line.as_bytes());
        }
    }
    history
}

fn close(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-8, "{a} != {b}");
}

#[test]
fn fault_and_recovery_inside_one_received_batch_still_breaks_cg_trail() {
    let mut panel = crate::balance_ui::Panel::default();
    panel.config = configured();
    let mut history = readings([1; 4]);
    panel.observe(&history, 1.2);
    let mut bytes = String::from("LC1 status=protocol\n");
    for _ in 0..3 {
        bytes.push_str("LC1 raw=-102000 status=no_tare\nLC2 raw=-98000 status=no_tare\nLC3 raw=-98000 status=no_tare\nLC4 raw=-98000 status=no_tare\n");
    }
    history.ingest(1.3, bytes.as_bytes());
    panel.observe(&history, 1.3);
    let segments = panel.trace.segments(1.3, 1.0);
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[0].len(), 1);
    assert_eq!(segments[1].len(), 1);
}

#[test]
fn centered_and_asymmetric_loads_use_raw_and_signed_pc_calibration() {
    let config = configured();
    let equal = config.calculate(&readings([1; 4]), 1.2).unwrap();
    close(equal.total_kg, 4.0);
    close(equal.x_mm, 0.0);
    close(equal.y_mm, 100.0);
    let offset = config.calculate(&readings([1, 2, 3, 4]), 1.2).unwrap();
    close(offset.total_kg, 10.0);
    close(offset.x_mm, -20.0);
    close(offset.y_mm, 140.0);
    assert!(!offset.nominal);
    assert_eq!(plot_position(-20.0, 140.0), [-20.0, -140.0]);
}

#[test]
fn requested_layout_formula_and_permuted_channel_assignment() {
    let mut config = configured();
    config.layout.points[2] = [120.0, 210.0];
    config.layout.points[3] = [-80.0, 190.0];
    assert_eq!(
        config.layout.positions(),
        [[100.0, 0.0], [-100.0, 0.0], [120.0, 210.0], [-80.0, 190.0]]
    );
    let cg = config.calculate(&readings([1, 2, 3, 4]), 1.2).unwrap();
    close(cg.x_mm, -6.0);
    close(cg.y_mm, 139.0);
    config.layout.channels = [2, 3, 0, 1];
    let swapped = config.calculate(&readings([3, 4, 1, 2]), 1.2).unwrap();
    close(swapped.x_mm, cg.x_mm);
    close(swapped.y_mm, cg.y_mm);
    config.layout.channels = [0, 0, 2, 3];
    assert!(config.calculate(&readings([1; 4]), 1.2).is_err());
}

#[test]
fn geometry_and_every_support_are_required() {
    let mut config = configured();
    config.layout.points = [[0.0, 0.0], [0.0, 100.0], [0.0, 200.0], [0.0, 300.0]];
    assert!(config.calculate(&readings([1; 4]), 1.2).is_err());
    config = configured();
    config.cells[3].enabled = false;
    let mut history = readings([1; 4]);
    history.ingest(1.2, b"LC4 status=timeout\n");
    assert!(config.calculate(&history, 1.2).is_ok());
    config.cells[2].enabled = false;
    assert!(config.calculate(&history, 1.2).is_err());
    config = configured();
    assert!(config.calculate(&history, 1.2).is_err());
}

#[test]
fn zero_total_missing_zero_negative_load_and_nominal_span_are_explicit() {
    let mut config = configured();
    assert!(config.calculate(&readings([0; 4]), 1.2).is_err());
    assert!(config.calculate(&readings([-1, 2, 3, 4]), 1.2).is_err());
    config.cells[0].zero_raw = None;
    assert!(config.calculate(&readings([1; 4]), 1.2).is_err());
    config = configured();
    config.cells[0].calibrated = false;
    assert!(config.calculate(&readings([1; 4]), 1.2).unwrap().nominal);
    config.cells[0].zero_raw = Some(-100_010.0);
    let cg = config.calculate(&readings([0, 1, 1, 1]), 1.2).unwrap();
    close(cg.loads[0].unwrap(), -0.01);
    close(cg.total_kg, 2.99);
}

#[test]
fn stale_skewed_faulted_and_gapped_samples_never_produce_current_cg() {
    let config = configured();
    assert!(config.calculate(&readings([1; 4]), 1.8).is_err());
    let mut skewed = readings([1; 4]);
    skewed.ingest(1.45, b"LC4 raw=-99000 status=no_tare\n");
    assert!(config.calculate(&skewed, 1.45).is_err());
    for line in [
        "LC1 status=timeout\n",
        "LC1 status=saturated\n",
        "LC1 status=protocol\n",
        "Dropped LC1 samples=2 (USB backlog)\n",
        "Remove all loads; tare starts in 3 seconds.\n",
    ] {
        let mut history = readings([1; 4]);
        history.ingest(1.2, line.as_bytes());
        assert!(config.calculate(&history, 1.2).is_err(), "{line}");
    }
}

#[test]
fn averaging_restarts_after_gap_fault_and_firmware_tare() {
    let config = configured();
    for line in [
        "LC1 status=timeout\n",
        "Dropped LC1 samples=2 (USB backlog)\n",
        "Remove all loads; tare starts in 3 seconds.\nTare complete.\n",
    ] {
        let mut history = readings([1; 4]);
        history.ingest(1.21, line.as_bytes());
        for (n, t) in [1.22, 1.23, 1.24].into_iter().enumerate() {
            for i in 0..4 {
                let raw = if i == 0 { -102000 } else { -98000 };
                history.ingest(
                    t,
                    format!("LC{} raw={raw} status=no_tare\n", i + 1).as_bytes(),
                );
            }
            if n < 2 {
                assert!(config.calculate(&history, t).is_err());
            }
        }
        close(mean(&history, &config, 0, 1.24).unwrap().raw, -102000.0);
        assert!(config.calculate(&history, 1.24).is_ok());
    }
}

#[test]
fn pc_tare_is_atomic_and_span_capture_infers_polarity() {
    let mut config = configured();
    let mut history = readings([1, 2, 3, 4]);
    config.zero_all(&history, 1.2).unwrap();
    assert!(config.calculate(&history, 1.2).is_err());
    let before = config.clone();
    history.ingest(1.2, b"LC4 status=protocol\n");
    assert!(config.zero_all(&history, 1.2).is_err());
    assert_eq!(config, before);
    let loaded = Mean {
        raw: -103000.0,
        count: 5,
        latest: 1.2,
    };
    config.calibrate(0, loaded, 2.0).unwrap();
    close(config.cells[0].counts_per_kg, -1000.0);
    assert!(config.cells[0].calibrated);
    let before = config.clone();
    for mass in [0.0, -1.0, f64::NAN] {
        assert!(config.calibrate(0, loaded, mass).is_err());
    }
    assert_eq!(config, before);
}

#[test]
fn settings_reject_nonfinite_zero_sensitivity_bad_version_and_saturation() {
    for value in [0.0, f64::NAN, f64::INFINITY] {
        let mut config = configured();
        config.cells[0].counts_per_kg = value;
        assert!(config.validate().is_err());
    }
    let mut config = configured();
    config.version = 99;
    assert!(config.validate().is_err());
    for raw in [-8388608, 8388607] {
        let mut history = readings([1; 4]);
        history.ingest(1.2, format!("LC1 raw={raw} status=no_tare\n").as_bytes());
        assert!(configured().calculate(&history, 1.2).is_err());
    }
}
