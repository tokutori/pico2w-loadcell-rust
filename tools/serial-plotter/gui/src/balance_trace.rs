//! Derived PC CG history. Never bridges an invalid interval or a settings change.
use crate::balance::{Balance, Config};
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug)]
pub struct Point {
    pub seconds: f64,
    pub x: f64,
    pub y: f64,
    pub break_before: bool,
}

#[derive(Default)]
pub struct Trace {
    pub points: VecDeque<Point>,
    config: Option<Config>,
    revision: Option<u64>,
    last_time: f64,
    gap: bool,
}

impl Trace {
    pub fn break_line(&mut self) {
        self.gap = true;
    }
    pub fn update(&mut self, config: &Config, revision: u64, now: f64, value: Option<&Balance>) {
        let mut calculation_config = config.clone();
        calculation_config.trace_seconds = 0.0; // view changes preserve history
        if self.config.as_ref() != Some(&calculation_config)
            || self.revision.is_some_and(|r| revision < r)
            || now < self.last_time
        {
            self.points.clear();
            self.revision = None;
            self.gap = true;
            self.config = Some(calculation_config);
        }
        self.last_time = now;
        while self.points.front().is_some_and(|p| p.seconds < now - 300.0) {
            self.points.pop_front();
        }
        if value.is_none() {
            self.gap = true;
        }
        if self.revision == Some(revision) {
            return;
        }
        self.revision = Some(revision);
        if let Some(value) = value {
            if self
                .points
                .back()
                .is_some_and(|p| now - p.seconds > config.max_age_s)
            {
                self.gap = true;
            }
            // Several chunks can carry the same PC timestamp. Keep their final
            // result, but retain any intervening invalidity as a line break.
            if self.points.back().is_some_and(|p| p.seconds == now)
                && let Some(previous) = self.points.pop_back()
            {
                self.gap |= previous.break_before;
            }
            self.points.push_back(Point {
                seconds: now,
                x: value.x_mm,
                y: value.y_mm,
                break_before: std::mem::take(&mut self.gap),
            });
            while self.points.len() > 24_000 {
                self.points.pop_front();
            }
        }
    }

    pub fn segments(&self, now: f64, window: f64) -> Vec<Vec<Point>> {
        let mut segments = Vec::new();
        let mut segment = Vec::new();
        for point in self
            .points
            .iter()
            .filter(|p| p.seconds >= now - window && p.seconds <= now)
        {
            if point.break_before && !segment.is_empty() {
                segments.push(std::mem::take(&mut segment));
            }
            segment.push(*point);
        }
        if !segment.is_empty() {
            segments.push(segment);
        }
        segments
    }
}

/// Oldest visible trail is thinner and more transparent than its recent end.
pub fn trail_style(age: f64) -> (f32, u8) {
    let recency = (1.0 - age).clamp(0.0, 1.0);
    ((0.4 + 2.1 * recency) as f32, (25.0 + 110.0 * recency) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn value(x: f64) -> Balance {
        Balance {
            total_kg: 10.0,
            x_mm: x,
            y_mm: 200.0,
            loads: [Some(2.5); 4],
            nominal: false,
        }
    }
    #[test]
    fn trail_expires_and_invalid_intervals_do_not_join() {
        let mut trace = Trace::default();
        let config = Config::default();
        for (rev, time) in [(1, 0.0), (2, 0.5), (3, 1.0), (4, 1.5)] {
            trace.update(&config, rev, time, Some(&value(time)));
        }
        assert_eq!(trace.segments(1.5, 1.0)[0].len(), 3);
        trace.update(&config, 5, 1.6, None);
        trace.update(&config, 6, 1.7, Some(&value(9.0)));
        let segments = trace.segments(1.7, 1.0);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].len(), 2);
        assert_eq!(segments[1].len(), 1);
        assert!(trace.segments(3.0, 1.0).is_empty());
        let old = trail_style(1.0);
        let new = trail_style(0.0);
        assert!(old.0 < new.0 && old.1 < new.1);
    }
    #[test]
    fn configuration_and_session_changes_clear_trace_but_repainting_does_not_add_data() {
        let mut trace = Trace::default();
        let mut config = Config::default();
        trace.update(&config, 1, 1.0, Some(&value(1.0)));
        trace.update(&config, 1, 1.1, Some(&value(1.0)));
        assert_eq!(trace.points.len(), 1);
        config.trace_seconds = 30.0;
        trace.update(&config, 2, 1.2, Some(&value(2.0)));
        assert_eq!(trace.points.len(), 2);
        config.layout.points[2][0] += 20.0;
        trace.update(&config, 2, 1.3, Some(&value(3.0)));
        assert_eq!(trace.points.len(), 1);
        trace.update(&config, 0, 0.0, None);
        assert!(trace.points.is_empty());
    }
}
