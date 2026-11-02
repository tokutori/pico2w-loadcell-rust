//! PC-only calibration and static horizontal center of gravity.
//! X is rightward, Y is rearward, in mm from the midpoint of the front pair.
use crate::history::{History, TareState, valid_raw};
use serde::{Deserialize, Serialize};
use serial_plotter_core::{Measurement, Metric};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CellConfig {
    pub enabled: bool,
    pub zero_raw: Option<f64>,
    pub counts_per_kg: f64,
    pub calibrated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub layout: Layout,
    pub cells: [CellConfig; 4],
    pub average_s: f64,
    pub min_samples: usize,
    pub max_age_s: f64,
    pub max_skew_s: f64,
    pub min_total_kg: f64,
    pub negative_tolerance_kg: f64,
    pub trace_seconds: f64,
    #[serde(default = "default_auto_zero")]
    pub auto_zero_on_connect: bool,
}

fn default_auto_zero() -> bool {
    true
}

/// Coordinates belong to named measurement points, independently of LC numbers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "StoredLayout")]
pub struct Layout {
    /// Upper-right, upper-left, lower-right, lower-left. +x right, +y rear.
    pub points: [[f64; 2]; 4],
    /// Named point -> zero-based physical LC index.
    pub channels: [usize; 4],
}

pub const POINT_NAMES: [&str; 4] = [
    "Upper right (front right)",
    "Upper left (front left)",
    "Lower right (rear right)",
    "Lower left (rear left)",
];
pub const POINT_TAGS: [&str; 4] = ["UR", "UL", "LR", "LL"];

/// Read the former dimension-based JSON without losing channel assignments.
/// New saves contain only points and channels. Mixed representations are rejected.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredLayout {
    points: Option<[[f64; 2]; 4]>,
    channels: [usize; 4],
    x1: Option<f64>,
    x2: Option<f64>,
    y: Option<f64>,
    dx: Option<f64>,
    dy: Option<f64>,
}

impl TryFrom<StoredLayout> for Layout {
    type Error = String;
    fn try_from(s: StoredLayout) -> Result<Self, String> {
        match (s.points, s.x1, s.x2, s.y, s.dx, s.dy) {
            (Some(points), None, None, None, None, None) => Ok(Self {
                points,
                channels: s.channels,
            }),
            (None, Some(x1), Some(x2), Some(y), Some(dx), Some(dy))
                if [x1, x2, y, dx, dy]
                    .iter()
                    .all(|v| v.is_finite() && v.abs() <= 1e6)
                    && x1 >= 0.0
                    && x2 >= 0.0
                    && y >= 0.0 =>
            {
                Ok(Self {
                    points: [[x1, 0.0], [-x1, 0.0], [x2 + dx, y + dy], [-x2 + dx, y - dy]],
                    channels: s.channels,
                })
            }
            _ => Err(
                "Layout requires points + channels, or a complete legacy dimension layout".into(),
            ),
        }
    }
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            points: [
                [300.0, 0.0],
                [-300.0, 0.0],
                [300.0, 2000.0],
                [-300.0, 2000.0],
            ],
            channels: [0, 1, 2, 3],
        }
    }
}

impl Layout {
    /// Assign a physical sensor to a point, swapping the previous assignment.
    /// Coordinates and per-sensor calibration are never modified.
    pub fn assign(&mut self, point: usize, channel: usize) -> Result<(), String> {
        let mut sorted = self.channels;
        sorted.sort_unstable();
        if point >= 4 || channel >= 4 || sorted != [0, 1, 2, 3] {
            return Err("Invalid point/channel assignment".into());
        }
        let other = self.channels.iter().position(|c| *c == channel).unwrap();
        self.channels.swap(point, other);
        Ok(())
    }

    /// Resolve point coordinates into physical sensor order for CG calculation.
    pub fn positions(&self) -> [[f64; 2]; 4] {
        let mut positions = [[0.0; 2]; 4];
        for (role, channel) in self.channels.iter().copied().enumerate() {
            if channel < 4 {
                positions[channel] = self.points[role];
            }
        }
        positions
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            layout: Layout::default(),
            cells: std::array::from_fn(|i| CellConfig {
                enabled: true,
                zero_raw: None,
                counts_per_kg: if i == 0 { -77_309.0 } else { 77_309.0 },
                calibrated: false,
            }),
            average_s: 0.5,
            min_samples: 3,
            max_age_s: 0.5,
            max_skew_s: 0.2,
            min_total_kg: 0.1,
            negative_tolerance_kg: 0.02,
            trace_seconds: 10.0,
            auto_zero_on_connect: true,
        }
    }
}

impl Config {
    /// Derive plotted values from raw using the same signed calibration as CG.
    pub fn value(&self, index: usize, measurement: Measurement, metric: Metric) -> Option<f64> {
        let raw = valid_raw(measurement)?;
        if metric == Metric::Raw {
            return Some(raw);
        }
        let cell = &self.cells[index];
        let delta = raw - cell.zero_raw?;
        let value = match metric {
            Metric::Raw => raw,
            Metric::Delta => delta,
            Metric::Kg => delta / cell.counts_per_kg,
        };
        value.is_finite().then_some(value)
    }
    /// Validate persisted numeric settings without requiring completed setup.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("Unsupported settings version".into());
        }
        let mut channels = self.layout.channels;
        channels.sort_unstable();
        if channels != [0, 1, 2, 3] {
            return Err("Assign each LC exactly once to the four support positions".into());
        }
        if self
            .layout
            .points
            .iter()
            .flatten()
            .any(|v| !v.is_finite() || v.abs() > 1e6)
        {
            return Err(
                "Measurement point coordinates must be finite and within +/- 1000000 mm".into(),
            );
        }
        for (i, c) in self.cells.iter().enumerate() {
            if !c.counts_per_kg.is_finite()
                || c.counts_per_kg.abs() < 1e-6
                || c.counts_per_kg.abs() > 1e12
                || c.zero_raw
                    .is_some_and(|z| !z.is_finite() || !(-8_388_608.0..=8_388_607.0).contains(&z))
            {
                return Err(format!(
                    "LC{}: invalid position, zero or signed counts/kg",
                    i + 1
                ));
            }
        }
        if !self.trace_seconds.is_finite() || !(1.0..=300.0).contains(&self.trace_seconds) {
            return Err("CG time graph window must be 1 to 300 seconds".into());
        }
        if !self.average_s.is_finite()
            || !(0.05..=10.0).contains(&self.average_s)
            || !(1..=800).contains(&self.min_samples)
            || !self.max_age_s.is_finite()
            || !(0.05..=5.0).contains(&self.max_age_s)
            || !self.max_skew_s.is_finite()
            || !(0.0..=5.0).contains(&self.max_skew_s)
            || !self.min_total_kg.is_finite()
            || !(0.001..=1e6).contains(&self.min_total_kg)
            || !self.negative_tolerance_kg.is_finite()
            || !(0.0..=100.0).contains(&self.negative_tolerance_kg)
        {
            return Err("Invalid averaging, freshness or load limits".into());
        }
        Ok(())
    }

    fn geometry(&self) -> Result<(), String> {
        let positions = self.layout.positions();
        let points: Vec<_> = self
            .cells
            .iter()
            .enumerate()
            .filter(|(_, c)| c.enabled)
            .map(|(i, _)| positions[i])
            .collect();
        if points.len() < 3 {
            return Err("Enable at least 3 non-collinear supports for 2D CG".into());
        }
        for a in 0..points.len() {
            for b in a + 1..points.len() {
                for c in b + 1..points.len() {
                    let ab = [points[b][0] - points[a][0], points[b][1] - points[a][1]];
                    let ac = [points[c][0] - points[a][0], points[c][1] - points[a][1]];
                    if (ab[0] * ac[1] - ab[1] * ac[0]).abs() > 1e-6 {
                        return Ok(());
                    }
                }
            }
        }
        Err("Enter measured support positions: supports must not lie on one line".into())
    }

    pub fn zero_all(&mut self, history: &History, now: f64) -> Result<(), String> {
        self.validate()?;
        let means = self.means(history, now)?;
        for (cell, mean) in self.cells.iter_mut().zip(means) {
            if let Some(mean) = mean {
                cell.zero_raw = Some(mean.raw);
            }
        }
        Ok(())
    }

    pub fn calibrate(&mut self, index: usize, mean: Mean, mass: f64) -> Result<(), String> {
        let zero = self.cells[index]
            .zero_raw
            .ok_or("Capture this sensor's zero first")?;
        if !mass.is_finite() || mass <= 0.0 || mass > 1e6 {
            return Err("Known mass must be positive and finite".into());
        }
        let delta = mean.raw - zero;
        if !delta.is_finite() || delta.abs() < 1.0 {
            return Err("Load change is less than 1 ADC count; check load and zero".into());
        }
        let mut candidate = self.clone();
        candidate.cells[index].counts_per_kg = delta / mass;
        candidate.cells[index].calibrated = true;
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    pub fn means(&self, history: &History, now: f64) -> Result<[Option<Mean>; 4], String> {
        let mut means = [None; 4];
        for (i, cell) in self.cells.iter().enumerate() {
            if cell.enabled {
                means[i] = Some(mean(history, self, i, now)?);
            }
        }
        let times: Vec<_> = means.iter().flatten().map(|m| m.latest).collect();
        if times.is_empty() {
            return Err("Enable the sensors supporting the object".into());
        }
        let oldest = times.iter().copied().fold(f64::INFINITY, f64::min);
        let newest = times.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        if newest - oldest > self.max_skew_s {
            return Err(
                "Sensor reception times are too far apart; waiting for aligned data".into(),
            );
        }
        Ok(means)
    }

    pub fn calculate(&self, history: &History, now: f64) -> Result<Balance, String> {
        self.validate()?;
        self.geometry()?;
        let means = self.means(history, now)?;
        let mut loads = [None; 4];
        let positions = self.layout.positions();
        let (mut total, mut mx, mut my) = (0.0, 0.0, 0.0);
        let mut nominal = false;
        for (i, cell) in self.cells.iter().enumerate() {
            if let Some(mean) = means[i] {
                let zero = cell
                    .zero_raw
                    .ok_or_else(|| format!("LC{}: capture platform-only zero first", i + 1))?;
                let kg = (mean.raw - zero) / cell.counts_per_kg;
                if kg < -self.negative_tolerance_kg {
                    return Err(format!(
                        "LC{}: negative load {kg:.3} kg; check polarity, zero and support contact",
                        i + 1
                    ));
                }
                // Small negative readings are retained, never clipped to zero.
                loads[i] = Some(kg);
                total += kg;
                mx += kg * positions[i][0];
                my += kg * positions[i][1];
                nominal |= !cell.calibrated;
            }
        }
        if total < self.min_total_kg {
            return Err(format!(
                "Total load {total:.3} kg is below the {:.3} kg limit",
                self.min_total_kg
            ));
        }
        let (x_mm, y_mm) = (mx / total, my / total);
        if !total.is_finite() || !x_mm.is_finite() || !y_mm.is_finite() {
            return Err("Calculation overflow; check calibration".into());
        }
        Ok(Balance {
            total_kg: total,
            x_mm,
            y_mm,
            loads,
            nominal,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Mean {
    pub raw: f64,
    pub count: usize,
    pub latest: f64,
}

/// Per-channel display remains available even when the combined CG is invalid.
/// Reception is inferred from fresh valid ADC data, not electrical continuity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SensorReading {
    Unavailable(&'static str),
    Pending(&'static str),
    Load { kg: f64, nominal: bool },
}

pub fn sensor_reading(
    history: &History,
    config: &Config,
    index: usize,
    now: f64,
    connected: bool,
    zero_pending: bool,
) -> SensorReading {
    use SensorReading::{Load, Pending, Unavailable};
    if !connected {
        return Unavailable("disconnected");
    }
    let Some(last) = history.samples[index].back() else {
        return Unavailable("no data");
    };
    if !now.is_finite() || now < last.seconds || now - last.seconds > config.max_age_s {
        return Unavailable("stale");
    }
    if let Measurement::Fault(fault) = last.measurement {
        return Unavailable(match fault {
            serial_plotter_core::Fault::Timeout => "timeout",
            serial_plotter_core::Fault::Saturated => "saturated",
            serial_plotter_core::Fault::Protocol => "protocol error",
        });
    }
    if valid_raw(last.measurement).is_none() {
        return Unavailable("invalid raw");
    }
    if history.has_pending_gap(index) || history.latest[index].is_none() {
        return Unavailable("data gap");
    }
    if config.validate().is_err() {
        return Pending("RX: check settings");
    }
    if zero_pending || config.cells[index].zero_raw.is_none() {
        return Pending("RX: zero needed");
    }
    let Ok(mean) = mean(history, config, index, now) else {
        return Pending("RX: averaging");
    };
    let cell = &config.cells[index];
    let kg = (mean.raw - cell.zero_raw.unwrap()) / cell.counts_per_kg;
    if !kg.is_finite() {
        return Pending("RX: check span");
    }
    Load {
        kg,
        nominal: !cell.calibrated,
    }
}

/// Uses only the latest contiguous healthy window, including valid no_tare raw.
/// PC reception times cannot establish simultaneous ADC sampling.
pub fn mean(history: &History, config: &Config, index: usize, now: f64) -> Result<Mean, String> {
    let error = |why: &str| format!("LC{}: {why}", index + 1);
    if history.tare == TareState::Running || history.latest[index].is_none() {
        return Err(error("waiting for readings after firmware tare"));
    }
    if history.has_pending_gap(index) {
        return Err(error("waiting for readings after a data gap"));
    }
    let samples = &history.samples[index];
    let last = samples.back().ok_or_else(|| error("no readings"))?;
    if !now.is_finite() || now < last.seconds || now - last.seconds > config.max_age_s {
        return Err(error("stale data; waiting for a fresh reading"));
    }
    let (mut sum, mut count) = (0.0, 0);
    for sample in samples.iter().rev() {
        if sample.seconds < now - config.average_s {
            break;
        }
        let Some(raw) = valid_raw(sample.measurement) else {
            break;
        };
        sum += raw;
        count += 1;
        if sample.break_before {
            break;
        }
    }
    if count < config.min_samples {
        let why = if matches!(last.measurement, Measurement::Fault(_)) {
            "sensor fault"
        } else {
            "not enough fresh, uninterrupted samples"
        };
        return Err(error(why));
    }
    Ok(Mean {
        raw: sum / count as f64,
        count,
        latest: last.seconds,
    })
}

#[derive(Clone, Debug)]
pub struct Balance {
    pub total_kg: f64,
    pub x_mm: f64,
    pub y_mm: f64,
    pub loads: [Option<f64>; 4],
    pub nominal: bool,
}

pub fn plot_position(x_mm: f64, y_mm: f64) -> [f64; 2] {
    [x_mm, -y_mm]
}
