use serial_plotter_core::*;

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
