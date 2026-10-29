//! Bounded presentation data, independent of windowing and serial drivers.
use serial_plotter_core::{Channel, Frame, Framer, Measurement, Metric, Reading, parse_line};
use std::collections::VecDeque;

pub const MAX_LINES: usize = 2_000;
pub const MAX_SAMPLES: usize = 10_000;

#[derive(Clone, Copy)]
pub struct Sample {
    pub seconds: f64,
    pub measurement: Measurement,
}

pub struct History {
    pub lines: VecDeque<String>,
    pub samples: [VecDeque<Sample>; 4],
    pub latest: [Option<Reading>; 4],
    pub line_count: u64,
    pub errors: u64,
    pub last_time: f64,
    framer: Framer<4096>,
}

impl Default for History {
    fn default() -> Self {
        Self {
            lines: VecDeque::new(),
            samples: std::array::from_fn(|_| VecDeque::new()),
            latest: [None; 4],
            line_count: 0,
            errors: 0,
            last_time: 0.0,
            framer: Framer::default(),
        }
    }
}

impl History {
    /// Clear displayed history while retaining a partially received line.
    pub fn clear(&mut self) {
        self.lines.clear();
        for samples in &mut self.samples {
            samples.clear();
        }
        self.latest = [None; 4];
        self.line_count = 0;
        self.errors = 0;
    }

    pub fn ingest(&mut self, seconds: f64, bytes: &[u8]) {
        self.last_time = seconds;
        for &byte in bytes {
            match self.framer.push(byte) {
                Frame::Pending => {}
                Frame::TooLong => {
                    self.errors += 1;
                    self.lines
                        .push_back("[monitor: overlong line discarded (4096-byte limit)]".into());
                }
                Frame::InvalidUtf8 => {
                    self.errors += 1;
                    self.lines.push_back("[monitor: invalid UTF-8 line]".into());
                }
                Frame::Line(line) => {
                    self.line_count += 1;
                    self.lines.push_back(line.to_owned());
                    match parse_line(line) {
                        Ok(Some(reading)) => {
                            let i = reading.channel.index();
                            self.latest[i] = Some(reading);
                            self.samples[i].push_back(Sample {
                                seconds,
                                measurement: reading.measurement,
                            });
                            if self.samples[i].len() > MAX_SAMPLES {
                                self.samples[i].pop_front();
                            }
                        }
                        Ok(None) => {}
                        Err(_) => self.errors += 1,
                    }
                }
            }
            if self.lines.len() > MAX_LINES {
                self.lines.pop_front();
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
        let mut segments = Vec::new();
        let mut segment = Vec::new();
        for sample in &self.samples[channel.index()] {
            if sample.seconds < start || sample.seconds > end {
                continue;
            }
            match sample.measurement.value(metric) {
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
