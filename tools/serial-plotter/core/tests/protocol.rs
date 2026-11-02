use serial_plotter_core::*;

#[test]
fn raw_only_firmware_preserves_signed_adc_without_inventing_calibration() {
    for raw in [-8_388_608, -155495, -1, 0, 8_388_607] {
        let m = parse_line(&format!("LC2 raw={raw}"))
            .unwrap()
            .unwrap()
            .measurement;
        assert_eq!(m, Measurement::Raw { raw });
        assert_eq!(m.value(Metric::Raw), Some(raw as f64));
        assert_eq!(m.value(Metric::Kg), None);
        assert_eq!(m.value(Metric::Delta), None);
    }
    for line in [
        "LC1 raw=",
        "LC1 raw=NaN",
        "LC1 raw=1 extra",
        "LC1 raw=1 delta=2",
        "LC1 status=other",
    ] {
        assert!(parse_line(line).is_err());
    }
    assert_eq!(
        parse_diagnostic("Session started. Raw output; zero and calibration are PC-side."),
        Ok(Some(Diagnostic::SessionStarted))
    );
}

#[test]
fn current_firmware_diagnostics_are_typed_without_inventing_measurements() {
    let lines = [
        (
            "Remove all loads; tare starts in 3 seconds.",
            Diagnostic::TareStarted,
        ),
        (
            "Tare complete. 'kg~' is NOMINAL, not calibrated.",
            Diagnostic::TareComplete,
        ),
        (
            "Tare LC3 failed: 12 valid samples required; reconnect unloaded.",
            Diagnostic::TareFailed {
                channel: Channel::Lc3,
                required: 12,
            },
        ),
        (
            "Dropped LC4 samples=4294967295 (USB backlog)",
            Diagnostic::Dropped {
                channel: Channel::Lc4,
                count: u32::MAX,
            },
        ),
        (
            "Dropped LC2 samples=17 (transport backlog)",
            Diagnostic::Dropped {
                channel: Channel::Lc2,
                count: 17,
            },
        ),
    ];
    for (line, diagnostic) in lines {
        assert_eq!(parse_diagnostic(line), Ok(Some(diagnostic)));
        assert_eq!(parse_line(line), Ok(None));
        let wire = format!("{line}\r\n");
        for split in 0..=wire.len() {
            let mut framer = Framer::<4096>::default();
            let mut decoded = Vec::new();
            for part in [&wire.as_bytes()[..split], &wire.as_bytes()[split..]] {
                for &byte in part {
                    if let Frame::Line(line) = framer.push(byte) {
                        decoded.push(parse_diagnostic(line).unwrap().unwrap());
                    }
                }
            }
            assert_eq!(decoded, [diagnostic]);
        }
    }
    for line in [
        "Pico 2 W / HX711 x4",
        "Acquisition: independent x4; LC1 kg polarity inverted.",
        "HX711 validation: ready-low + idle-high; timing guard + recovery.",
    ] {
        assert_eq!(parse_diagnostic(line), Ok(None));
        assert_eq!(parse_line(line), Ok(None));
    }
}

#[test]
fn malformed_diagnostics_do_not_change_counts_or_tare_state() {
    for line in [
        "Dropped LC0 samples=1 (USB backlog)",
        "Dropped LC1 samples=0 (USB backlog)",
        "Dropped LC1 samples=-1 (USB backlog)",
        "Dropped LC1 samples=4294967296 (USB backlog)",
        "Dropped LC1 samples=5 (USB backlog) extra",
        "Dropped LC1 samples=5",
        "Tare LC2 failed: 0 valid samples required; reconnect unloaded.",
        "Tare LC5 failed: 12 valid samples required; reconnect unloaded.",
        "Tare LC2 failed: 12 valid samples required; reconnect unloaded. extra",
    ] {
        assert!(parse_diagnostic(line).is_err(), "{line}");
    }
}

#[test]
fn supplied_log_preserves_outliers_and_negative_values() {
    for (line, raw, delta, kg) in [
        (
            "LC1 raw=-155256 delta=-13743 kg~=-0.178",
            -155256,
            -13743,
            -0.178,
        ),
        ("LC1 raw=-1 delta=141512 kg~=1.830", -1, 141512, 1.830),
        (
            "LC1 raw=-1585505 delta=-1443992 kg~=-18.678",
            -1585505,
            -1443992,
            -18.678,
        ),
        ("LC4 raw=0 delta=0 kg~=0.000", 0, 0, 0.0),
    ] {
        let reading = parse_line(line).unwrap().unwrap();
        assert_eq!(reading.measurement, Measurement::Sample { raw, delta, kg });
    }
}

#[test]
fn faults_and_missing_tare_do_not_invent_weight() {
    for channel in Channel::ALL {
        let line = format!("{} status=timeout", channel.label());
        let reading = parse_line(&line).unwrap().unwrap();
        assert_eq!(reading.channel, channel);
        for metric in Metric::ALL {
            assert_eq!(reading.measurement.value(metric), None);
        }
    }
    let reading = parse_line("LC1 raw=42 status=no_tare").unwrap().unwrap();
    assert_eq!(reading.measurement.value(Metric::Raw), Some(42.0));
    assert_eq!(reading.measurement.value(Metric::Kg), None);
    assert_eq!(
        parse_line("LC2 status=saturated")
            .unwrap()
            .unwrap()
            .measurement,
        Measurement::Fault(Fault::Saturated)
    );
}

#[test]
fn handshake_fault_is_missing_for_every_channel_and_metric() {
    for channel in Channel::ALL {
        let reading = parse_line(&format!("{} status=protocol", channel.label()))
            .unwrap()
            .unwrap();
        assert_eq!(reading.channel, channel);
        assert_eq!(reading.measurement, Measurement::Fault(Fault::Protocol));
        for metric in Metric::ALL {
            assert_eq!(reading.measurement.value(metric), None);
        }
    }
    assert!(parse_line("LC1 status=protocol extra").is_err());
}

#[test]
fn malformed_and_unrelated_lines_are_separated() {
    for line in [
        "",
        "Tare complete.",
        "**** Failed to open the serial port COM5 ****",
    ] {
        assert_eq!(parse_line(line), Ok(None));
    }
    for line in [
        "LC0 status=timeout",
        "LC5 status=timeout",
        "LC1 raw=1 delta=2",
        "LC1 raw=foo delta=0 kg~=0",
        "LC1 raw=2147483648 delta=0 kg~=0",
        "LC1 raw=1 delta=0 kg~=NaN",
        "LC1 raw=1 delta=0 kg~=inf",
        "LC1 raw=1 delta=0 kg~=1e999",
        "LC1 raw=1 delta=0 kg~=0 extra",
        "LC1 status=timeout extra",
        "LC1 status=unknown",
        "LC1 raw=1 raw=1 kg~=0",
    ] {
        assert!(parse_line(line).is_err(), "{line}");
    }
    assert!(
        parse_line(" LC3\traw=-8388608 delta=-4294967295 kg~=-1.2e3\r\n")
            .unwrap()
            .is_some()
    );
}

#[test]
fn every_possible_read_split_preserves_crlf_lines() {
    let input = b"LC1 raw=-1 delta=141512 kg~=1.830\r\nLC2 status=timeout\n";
    for split in 0..=input.len() {
        let mut framer = Framer::<128>::default();
        let mut readings = Vec::new();
        for chunk in [&input[..split], &input[split..]] {
            for &byte in chunk {
                if let Frame::Line(line) = framer.push(byte) {
                    readings.push(parse_line(line).unwrap().unwrap());
                }
            }
        }
        assert_eq!(readings.len(), 2);
        assert_eq!(readings[0].measurement.value(Metric::Raw), Some(-1.0));
        assert_eq!(readings[1].measurement, Measurement::Fault(Fault::Timeout));
    }
}

#[test]
fn corrupt_and_overlong_lines_resynchronize_at_newline() {
    let mut framer = Framer::<4>::default();
    for &b in b"1234" {
        assert_eq!(framer.push(b), Frame::Pending);
    }
    assert_eq!(framer.push(b'5'), Frame::TooLong);
    for &b in b"more\n" {
        assert_eq!(framer.push(b), Frame::Pending);
    }
    assert_eq!(framer.push(255), Frame::Pending);
    assert_eq!(framer.push(b'\n'), Frame::InvalidUtf8);
    assert_eq!(framer.push(b'a'), Frame::Pending);
    assert_eq!(framer.push(b'\n'), Frame::Line("a"));
}
