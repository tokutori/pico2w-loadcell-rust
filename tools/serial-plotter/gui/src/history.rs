//! Bounded presentation data, independent of windowing and serial drivers.
use serial_plotter_core::{
    Channel, Diagnostic, Frame, Framer, Measurement, Metric, Reading, parse_diagnostic, parse_line,
};
use std::collections::VecDeque;

pub const MAX_LINES: usize = 2_000;
pub const MAX_SAMPLES: usize = 10_000;

/// A usable ADC word, independent of firmware tare and nominal kg output.
/// Keep negative/zero/-1 values; reject fault and saturation codes.
pub fn valid_raw(measurement: Measurement) -> Option<f64> {
    measurement
        .value(Metric::Raw)
        .filter(|raw| *raw > -8_388_608.0 && *raw < 8_388_607.0)
}

#[derive(Clone, Copy)]
pub struct Sample {
    pub seconds: f64,
    pub measurement: Measurement,
    pub break_before: bool,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub enum TareState {
    #[default]
    Unknown,
    Running,
    Complete,
}

pub struct History {
    pub lines: VecDeque<String>,
    pub samples: [VecDeque<Sample>; 4],
    pub latest: [Option<Reading>; 4],
    pub line_count: u64,
    /// Measurement identity is independent of log lines and clearing the display.
    pub measurement_revision: u64,
    pub measurement_time: f64,
    pub errors: u64,
    pub last_time: f64,
    pub tare: TareState,
    pub tare_failures: [Option<u16>; 4],
    pub dropped: [u64; 4],
    pub discontinuities: [u64; 4],
    pub session_revision: u64,
    gap_pending: [bool; 4],
    framer: Framer<4096>,
}

impl Default for History {
    fn default() -> Self {
        Self {
            lines: VecDeque::new(),
            samples: std::array::from_fn(|_| VecDeque::new()),
            latest: [None; 4],
            line_count: 0,
            measurement_revision: 0,
            measurement_time: 0.0,
            errors: 0,
            last_time: 0.0,
            tare: TareState::Unknown,
            tare_failures: [None; 4],
            dropped: [0; 4],
            discontinuities: [0; 4],
            session_revision: 0,
            gap_pending: [false; 4],
            framer: Framer::default(),
        }
    }
}

impl History {
    /// Unknown corruption may affect any channel. Invalidate averages as well
    /// as plots until each channel establishes a new contiguous sample window.
    fn invalidate_all(&mut self) {
        self.gap_pending = [true; 4];
        self.discontinuities
            .iter_mut()
            .for_each(|n| *n = n.wrapping_add(1));
    }

    pub fn transport_gap(&mut self) {
        self.framer = Framer::default();
        self.invalidate_all();
        self.errors += 1;
        self.lines
            .push_back("[monitor: BLE fragment lost; incomplete message discarded]".into());
        if self.lines.len() > MAX_LINES {
            self.lines.pop_front();
        }
    }
    /// Count actual received measurements in (now - 1 s, now], not a nominal
    /// device rate or frame rate. Diagnostics and malformed lines are absent.
    pub fn valid_samples_last_second(&self, now: f64) -> [usize; 4] {
        if !now.is_finite() {
            return [0; 4];
        }
        std::array::from_fn(|i| {
            self.samples[i]
                .iter()
                .rev()
                .take_while(|s| s.seconds > now - 1.0)
                .filter(|s| s.seconds <= now && valid_raw(s.measurement).is_some())
                .count()
        })
    }

    pub fn has_pending_gap(&self, channel: usize) -> bool {
        self.gap_pending[channel]
    }

    /// Clear displayed history while retaining a partially received line.
    pub fn clear(&mut self) {
        self.lines.clear();
        for samples in &mut self.samples {
            samples.clear();
        }
        self.latest = [None; 4];
        self.line_count = 0;
        self.errors = 0;
        self.dropped = [0; 4];
        // Calibration belongs to the firmware session, not displayed history.
    }

    pub fn ingest(&mut self, seconds: f64, bytes: &[u8]) {
        self.last_time = seconds;
        for &byte in bytes {
            match self.framer.push(byte) {
                Frame::Pending => {}
                Frame::TooLong => {
                    self.invalidate_all();
                    self.errors += 1;
                    self.lines
                        .push_back("[monitor: overlong line discarded (4096-byte limit)]".into());
                }
                Frame::InvalidUtf8 => {
                    self.invalidate_all();
                    self.errors += 1;
                    self.lines.push_back("[monitor: invalid UTF-8 line]".into());
                }
                Frame::Line(line) => {
                    self.line_count += 1;
                    self.lines.push_back(line.to_owned());
                    match parse_line(line) {
                        Ok(Some(reading)) => {
                            self.measurement_revision = self.measurement_revision.wrapping_add(1);
                            self.measurement_time = seconds;
                            let i = reading.channel.index();
                            if valid_raw(reading.measurement).is_none() {
                                self.discontinuities[i] = self.discontinuities[i].wrapping_add(1);
                            }
                            self.latest[i] = Some(reading);
                            self.samples[i].push_back(Sample {
                                seconds,
                                measurement: reading.measurement,
                                break_before: std::mem::take(&mut self.gap_pending[i]),
                            });
                            if self.samples[i].len() > MAX_SAMPLES {
                                self.samples[i].pop_front();
                            }
                        }
                        Ok(None) => match parse_diagnostic(line) {
                            Ok(Some(diagnostic)) => self.diagnostic(diagnostic),
                            Ok(None) => {}
                            Err(_) => {
                                self.errors += 1;
                                self.invalidate_all();
                            }
                        },
                        Err(_) => {
                            self.errors += 1;
                            self.invalidate_all();
                        }
                    }
                }
            }
            if self.lines.len() > MAX_LINES {
                self.lines.pop_front();
            }
        }
    }

    fn diagnostic(&mut self, diagnostic: Diagnostic) {
        match diagnostic {
            Diagnostic::SessionStarted => {
                self.session_revision = self.session_revision.wrapping_add(1);
                self.tare = TareState::Unknown;
                self.tare_failures = [None; 4];
                self.latest = [None; 4];
                self.gap_pending = [true; 4];
                self.discontinuities
                    .iter_mut()
                    .for_each(|n| *n = n.wrapping_add(1));
            }
            Diagnostic::TareStarted => {
                self.discontinuities
                    .iter_mut()
                    .for_each(|n| *n = n.wrapping_add(1));
                self.tare = TareState::Running;
                self.tare_failures = [None; 4];
                self.latest = [None; 4];
                self.gap_pending = [true; 4];
            }
            Diagnostic::TareComplete => self.tare = TareState::Complete,
            Diagnostic::TareFailed { channel, required } => {
                self.tare_failures[channel.index()] = Some(required);
            }
            Diagnostic::Dropped { channel, count } => {
                let i = channel.index();
                self.discontinuities[i] = self.discontinuities[i].wrapping_add(1);
                self.dropped[i] = self.dropped[i].saturating_add(u64::from(count));
                self.gap_pending[i] = true;
            }
        }
    }

    pub fn pending(&self) -> String {
        String::from_utf8_lossy(self.framer.pending_bytes()).into_owned()
    }

    pub fn monitor_text(&self) -> String {
        let mut text = String::new();
        for line in &self.lines {
            text.push_str(line);
            text.push('\n');
        }
        text.push_str(&self.pending());
        text
    }

    /// Split at missing values so timeout/no_tare is never interpolated as data.
    pub fn segments(
        &self,
        channel: Channel,
        metric: Metric,
        start: f64,
        end: f64,
    ) -> Vec<Vec<[f64; 2]>> {
        self.segments_with(channel, start, end, |m| m.value(metric))
    }

    pub fn segments_with(
        &self,
        channel: Channel,
        start: f64,
        end: f64,
        value: impl Fn(Measurement) -> Option<f64>,
    ) -> Vec<Vec<[f64; 2]>> {
        let mut segments = Vec::new();
        let mut segment = Vec::new();
        for sample in &self.samples[channel.index()] {
            if sample.seconds < start || sample.seconds > end {
                continue;
            }
            if sample.break_before && !segment.is_empty() {
                segments.push(std::mem::take(&mut segment));
            }
            match value(sample.measurement) {
                Some(value) => segment.push([sample.seconds, value]),
                None if !segment.is_empty() => segments.push(std::mem::take(&mut segment)),
                None => {}
            }
        }
        if !segment.is_empty() {
            segments.push(segment);
        }
        segments
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolling_one_second_counts_valid_raw_per_sensor_and_expires_without_input() {
        let mut history = History::default();
        history.ingest(1.0, b"LC1 raw=1 status=no_tare\n"); // excluded lower boundary
        history.ingest(
            1.01,
            b"LC1 raw=-1 status=no_tare\nLC2 raw=0 delta=0 kg~=0\n",
        );
        history.ingest(1.5, b"LC1 status=timeout\nLC2 status=protocol\nLC3 status=saturated\nLC4 raw=8388607 status=no_tare\nLC4 raw=-8388608 status=no_tare\nLC1 malformed\nDropped LC2 samples=5 (USB backlog)\n");
        history.ingest(2.0, b"LC1 raw=-123 delta=0 kg~=0\nLC3 raw=5 status=no_tare\nLC3 raw=6 status=no_tare\nLC4 raw=7 status=no_tare\n");
        assert_eq!(history.valid_samples_last_second(2.0), [2, 1, 2, 1]);
        assert_eq!(history.valid_samples_last_second(3.0), [0; 4]);
        history.clear();
        assert_eq!(history.valid_samples_last_second(2.0), [0; 4]);
    }

    #[test]
    fn tare_failure_and_dropped_samples_remain_visible_and_split_only_affected_channel() {
        let mut history = History::default();
        history.ingest(0.0, b"Remove all loads; tare starts in 3 seconds.\r\n");
        assert_eq!(history.tare, TareState::Running);
        history.ingest(3.5, b"Tare LC3 failed: 12 valid samples required; reconnect unloaded.\r\nTare complete. 'kg~' is NOMINAL, not calibrated.\r\n");
        assert_eq!(history.tare, TareState::Complete);
        assert_eq!(history.tare_failures, [None, None, Some(12), None]);
        history.ingest(4.0, b"LC2 raw=2 delta=2 kg~=2\nLC1 raw=-1 delta=-1 kg~=1\nLC3 raw=9 status=no_tare\nLC4 status=protocol\n");
        history.ingest(
            4.1,
            b"Dropped LC1 samples=17 (USB backlog)\nDropped LC1 samples=3 (USB backlog)\n",
        );
        history.ingest(4.2, b"LC1 raw=-2 delta=-2 kg~=2\nLC2 raw=3 delta=3 kg~=3\n");
        assert_eq!(history.errors, 0);
        assert_eq!(history.dropped, [20, 0, 0, 0]);
        for metric in Metric::ALL {
            assert_eq!(history.segments(Channel::Lc1, metric, 0.0, 5.0).len(), 2);
            assert_eq!(history.segments(Channel::Lc2, metric, 0.0, 5.0).len(), 1);
            assert!(history.segments(Channel::Lc4, metric, 0.0, 5.0).is_empty());
        }
        assert!(
            history
                .segments(Channel::Lc3, Metric::Kg, 0.0, 5.0)
                .is_empty()
        );
        assert_eq!(history.samples[0].len(), 2); // diagnostics are not fake samples
        history.clear();
        assert_eq!(history.tare_failures[2], Some(12));
        assert_eq!(history.dropped, [0; 4]);
        history.ingest(6.0, b"Remove all loads; tare starts in 3 seconds.\n");
        assert_eq!(history.tare_failures, [None; 4]);
        assert_eq!(history.tare, TareState::Running);
    }

    #[test]
    fn four_channel_10_and_80_sps_streams_keep_every_sample_until_retention_limit() {
        for rate in [10, 80] {
            let mut history = History::default();
            let count = MAX_SAMPLES + 400;
            let frames = count * 20 / rate; // receive batches at 20 GUI frames/s
            let began = std::time::Instant::now();
            for frame in 0..frames {
                let mut bytes = String::new();
                for sample in frame * rate / 20..(frame + 1) * rate / 20 {
                    // Independent arrival order; never assume LC1-LC4 batches.
                    for offset in 0..4 {
                        let channel = (sample + offset) % 4;
                        let raw = if channel == 0 {
                            -(sample as i32)
                        } else {
                            sample as i32
                        };
                        bytes.push_str(&format!(
                            "LC{} raw={raw} delta={raw} kg~={}\r\n",
                            channel + 1,
                            sample
                        ));
                    }
                }
                let fragment = [1, 7, 64, 4096][frame % 4];
                for chunk in bytes.as_bytes().chunks(fragment) {
                    history.ingest(frame as f64 / 20.0, chunk);
                }
            }
            assert_eq!(history.line_count, (count * 4) as u64);
            assert_eq!(history.errors, 0);
            assert_eq!(history.lines.len(), MAX_LINES);
            for channel in Channel::ALL {
                let samples = &history.samples[channel.index()];
                assert_eq!(samples.len(), MAX_SAMPLES);
                let sign = if channel == Channel::Lc1 { -1.0 } else { 1.0 };
                for (i, sample) in samples.iter().enumerate() {
                    assert_eq!(
                        sample.measurement.value(Metric::Raw),
                        Some(sign * (400 + i) as f64)
                    );
                }
                let segments = history.segments(
                    channel,
                    Metric::Raw,
                    history.last_time - 60.0,
                    history.last_time,
                );
                assert_eq!(segments.len(), 1);
                // Inclusive viewport edges may add one receive frame of data.
                assert!((rate * 60..=rate * 60 + rate / 20 + 1).contains(&segments[0].len()));
            }
            eprintln!(
                "{rate} SPS x4: {} lines, simulated {:.1}s, ingest {:.3}s",
                count * 4,
                count as f64 / rate as f64,
                began.elapsed().as_secs_f64()
            );
        }
    }

    #[test]
    fn monitor_keeps_all_lines_but_only_lc_data_is_plotted() {
        let mut history = History::default();
        history.ingest(0.0, b"boot\r\nLC1 raw=-1 del");
        assert!(history.samples[0].is_empty());
        assert!(history.monitor_text().ends_with("LC1 raw=-1 del"));
        history.ingest(
            1.0,
            b"ta=141512 kg~=1.830\r\nLC2 status=timeout\nerror text\n",
        );
        assert_eq!(history.lines.len(), 4);
        assert_eq!(history.samples[0].len(), 1);
        assert_eq!(history.samples[1].len(), 1);
        assert_eq!(history.errors, 0);
        assert!(
            history
                .segments(Channel::Lc2, Metric::Raw, 0.0, 2.0)
                .is_empty()
        );
    }

    #[test]
    fn faults_split_lines_and_untared_samples_only_plot_raw() {
        let mut history = History::default();
        for (i, line) in [
            "LC1 raw=1 delta=2 kg~=3\n",
            "LC1 status=timeout\n",
            "LC1 raw=4 status=no_tare\n",
            "LC1 raw=5 delta=6 kg~=7\n",
        ]
        .iter()
        .enumerate()
        {
            history.ingest(i as f64, line.as_bytes());
        }
        assert_eq!(
            history.segments(Channel::Lc1, Metric::Raw, 0.0, 3.0),
            vec![vec![[0.0, 1.0]], vec![[2.0, 4.0], [3.0, 5.0]]]
        );
        assert_eq!(
            history.segments(Channel::Lc1, Metric::Kg, 0.0, 3.0),
            vec![vec![[0.0, 3.0]], vec![[3.0, 7.0]]]
        );
    }

    #[test]
    fn histories_are_bounded_and_keep_the_latest_data() {
        let mut history = History::default();
        for i in 0..MAX_SAMPLES + 5 {
            history.ingest(i as f64, b"LC1 raw=1 delta=0 kg~=0\n");
        }
        assert_eq!(history.samples[0].len(), MAX_SAMPLES);
        assert_eq!(history.lines.len(), MAX_LINES);
        assert_eq!(history.samples[0].front().unwrap().seconds, 5.0);
    }
}
