//! PC-only calibration and static horizontal center of gravity.
//! X is rightward, Y is rearward, in mm from the midpoint of the front pair.
pub mod error;
pub use error::{DataIssue, Error, WaitReason};
pub mod settings;
#[cfg(test)]
mod tests;
pub mod trace;
pub mod ui;
mod zero;

use crate::history::{History, TareState, valid_raw};
use serde::{Deserialize, Serialize};
use serial_plotter_core::{Channel, Measurement, Metric};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CellConfig {
    pub enabled: bool,
    pub zero_raw: Option<f64>,
    pub counts_per_kg: f64,
    pub calibrated: bool,
}

impl CellConfig {
    fn valid(&self) -> bool {
        self.counts_per_kg.is_finite()
            && (1e-6..=1e12).contains(&self.counts_per_kg.abs())
            && self
                .zero_raw
                .is_none_or(|z| z.is_finite() && (-8_388_608.0..=8_388_607.0).contains(&z))
    }
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
    pub channels: Assignment,
}

/// A bijection: the four physical channels occur exactly once. JSON is checked
/// at this boundary, so rendering can never index with an invalid assignment.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "[usize; 4]", into = "[usize; 4]")]
pub struct Assignment([usize; 4]);
impl Default for Assignment {
    fn default() -> Self {
        Self([0, 1, 2, 3])
    }
}
impl TryFrom<[usize; 4]> for Assignment {
    type Error = Error;
    fn try_from(value: [usize; 4]) -> Result<Self, Error> {
        let mut sorted = value;
        sorted.sort_unstable();
        if sorted != [0, 1, 2, 3] {
            return Err(Error::Assignment);
        }
        Ok(Self(value))
    }
}
impl From<Assignment> for [usize; 4] {
    fn from(value: Assignment) -> Self {
        value.0
    }
}
impl std::ops::Index<usize> for Assignment {
    type Output = usize;
    fn index(&self, point: usize) -> &usize {
        &self.0[point]
    }
}
impl Assignment {
    pub fn iter(&self) -> std::slice::Iter<'_, usize> {
        self.0.iter()
    }
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
    channels: Assignment,
    x1: Option<f64>,
    x2: Option<f64>,
    y: Option<f64>,
    dx: Option<f64>,
    dy: Option<f64>,
}

impl TryFrom<StoredLayout> for Layout {
    type Error = Error;
    fn try_from(s: StoredLayout) -> Result<Self, Error> {
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
            _ => Err(Error::LayoutFormat),
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
            channels: Assignment::default(),
        }
    }
}

impl Layout {
    /// Assign a physical sensor to a point, swapping the previous assignment.
    /// Coordinates and per-sensor calibration are never modified.
    pub fn assign(&mut self, point: usize, channel: usize) -> Result<(), Error> {
        if point >= 4 || channel >= 4 {
            return Err(Error::Assignment);
        }
        let other = self
            .channels
            .0
            .iter()
            .position(|c| *c == channel)
            .ok_or(Error::Assignment)?;
        self.channels.0.swap(point, other);
        Ok(())
    }

    /// Resolve point coordinates into physical sensor order for CG calculation.
    pub fn positions(&self) -> [[f64; 2]; 4] {
        let mut positions = [[0.0; 2]; 4];
        for (role, channel) in self.channels.iter().copied().enumerate() {
            positions[channel] = self.points[role];
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

/// Only inputs that alter CG results belong in its history identity.
#[derive(Clone, Debug, PartialEq)]
pub struct CalculationKey {
    layout: Layout,
    cells: [CellConfig; 4],
    average_s: f64,
    min_samples: usize,
    max_age_s: f64,
    max_skew_s: f64,
    min_total_kg: f64,
    negative_tolerance_kg: f64,
}

impl Config {
    pub fn calculation_key(&self) -> CalculationKey {
        CalculationKey {
            layout: self.layout.clone(),
            cells: self.cells.clone(),
            average_s: self.average_s,
            min_samples: self.min_samples,
            max_age_s: self.max_age_s,
            max_skew_s: self.max_skew_s,
            min_total_kg: self.min_total_kg,
            negative_tolerance_kg: self.negative_tolerance_kg,
        }
    }

    /// Derive plotted values from raw using the same signed calibration as CG.
    pub fn value(&self, index: usize, measurement: Measurement, metric: Metric) -> Option<f64> {
        let raw = valid_raw(measurement)?;
        if metric == Metric::Raw {
            return Some(raw);
        }
        let cell = &self.cells[index];
        if !cell.valid() {
            return None;
        }
        let delta = raw - cell.zero_raw?;
        let value = match metric {
            Metric::Raw => raw,
            Metric::Delta => delta,
            Metric::Kg => delta / cell.counts_per_kg,
        };
        value.is_finite().then_some(value)
    }
    /// Validate persisted numeric settings without requiring completed setup.
    pub fn validate(&self) -> Result<(), Error> {
        if self.version != 1 {
            return Err(Error::Version(self.version));
        }
        if self
            .layout
            .points
            .iter()
            .flatten()
            .any(|v| !v.is_finite() || v.abs() > 1e6)
        {
            return Err(Error::Coordinates);
        }
        for (i, c) in self.cells.iter().enumerate() {
            if !c.valid() {
                return Err(Error::Calibration(Channel::ALL[i]));
            }
        }
        if !self.trace_seconds.is_finite() || !(1.0..=300.0).contains(&self.trace_seconds) {
            return Err(Error::TraceWindow);
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
            return Err(Error::Limits);
        }
        Ok(())
    }

    fn geometry(&self) -> Result<(), Error> {
        let positions = self.layout.positions();
        let points: Vec<_> = self
            .cells
            .iter()
            .enumerate()
            .filter(|(_, c)| c.enabled)
            .map(|(i, _)| positions[i])
            .collect();
        if points.len() < 3 {
            return Err(Error::TooFewSupports);
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
        Err(Error::Collinear)
    }

    pub fn zero_all(&mut self, history: &History, now: f64) -> Result<(), Error> {
        self.validate()?;
        let means = self.means(history, now)?;
        for (cell, mean) in self.cells.iter_mut().zip(means) {
            if let Some(mean) = mean {
                cell.zero_raw = Some(mean.raw);
            }
        }
        Ok(())
    }

    pub fn calibrate(&mut self, index: usize, mean: Mean, mass: f64) -> Result<(), Error> {
        let zero = self.cells[index]
            .zero_raw
            .ok_or(Error::ZeroMissing(Channel::ALL[index]))?;
        if !mass.is_finite() || mass <= 0.0 || mass > 1e6 {
            return Err(Error::KnownMass);
        }
        let delta = mean.raw - zero;
        if !delta.is_finite() || delta.abs() < 1.0 {
            return Err(Error::LoadChange);
        }
        let mut candidate = self.clone();
        candidate.cells[index].counts_per_kg = delta / mass;
        candidate.cells[index].calibrated = true;
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    pub fn means(&self, history: &History, now: f64) -> Result<[Option<Mean>; 4], Error> {
        let mut means = [None; 4];
        for (i, cell) in self.cells.iter().enumerate() {
            if cell.enabled {
                means[i] = Some(mean(history, self, i, now)?);
            }
        }
        let times: Vec<_> = means.iter().flatten().map(|m| m.latest).collect();
        if times.is_empty() {
            return Err(Error::NoEnabledSensors);
        }
        let oldest = times.iter().copied().fold(f64::INFINITY, f64::min);
        let newest = times.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        if newest - oldest > self.max_skew_s {
            return Err(Error::TimeSkew);
        }
        Ok(means)
    }

    /// Sum all four calibrated supports independently of CG geometry or its
    /// minimum-load threshold. A missing/disabled support is never treated as zero.
    /// Calibration is mass-equivalent; kgf uses the standard-gravity convention.
    pub fn total_load(&self, history: &History, now: f64) -> Result<TotalLoad, Error> {
        for (i, cell) in self.cells.iter().enumerate() {
            if !cell.enabled {
                return Err(Error::DisabledSensor(Channel::ALL[i]));
            }
        }
        let loads = self.load_summary(history, now)?;
        Ok(TotalLoad {
            kgf: loads.total,
            nominal: loads.nominal,
        })
    }

    fn load_summary(&self, history: &History, now: f64) -> Result<Loads, Error> {
        self.validate()?;
        let means = self.means(history, now)?;
        let mut loads = [None; 4];
        let mut total = 0.0;
        let mut nominal = false;
        for (i, cell) in self.cells.iter().enumerate() {
            if let Some(mean) = means[i] {
                let zero = cell.zero_raw.ok_or(Error::ZeroMissing(Channel::ALL[i]))?;
                let kg = (mean.raw - zero) / cell.counts_per_kg;
                if kg < -self.negative_tolerance_kg {
                    return Err(Error::NegativeLoad {
                        channel: Channel::ALL[i],
                        kg,
                    });
                }
                // Small negative readings are retained, never clipped to zero.
                loads[i] = Some(kg);
                total += kg;
                nominal |= !cell.calibrated;
            }
        }
        if !total.is_finite() {
            return Err(Error::Overflow);
        }
        Ok(Loads {
            total,
            loads,
            nominal,
        })
    }

    pub fn calculate(&self, history: &History, now: f64) -> Result<Balance, Error> {
        self.validate()?;
        self.geometry()?;
        let Loads {
            total,
            loads,
            nominal,
        } = self.load_summary(history, now)?;
        let positions = self.layout.positions();
        let (mut mx, mut my) = (0.0, 0.0);
        for (kg, [x, y]) in loads.iter().zip(positions) {
            if let Some(kg) = kg {
                mx += kg * x;
                my += kg * y;
            }
        }
        if total < self.min_total_kg {
            return Err(Error::LowTotal {
                kg: total,
                minimum: self.min_total_kg,
            });
        }
        let (x_mm, y_mm) = (mx / total, my / total);
        if !total.is_finite() || !x_mm.is_finite() || !y_mm.is_finite() {
            return Err(Error::Overflow);
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

struct Loads {
    total: f64,
    loads: [Option<f64>; 4],
    nominal: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TotalLoad {
    pub kgf: f64,
    pub nominal: bool,
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
    Unavailable(DataIssue),
    Pending(WaitReason),
    Load { kg: f64, nominal: bool },
}

fn available(history: &History, config: &Config, index: usize, now: f64) -> Result<(), DataIssue> {
    if history.tare == TareState::Running {
        return Err(DataIssue::FirmwareTare);
    }
    if history.has_pending_gap(index) {
        return Err(DataIssue::Gap);
    }
    let last = history.samples[index]
        .back()
        .filter(|_| history.latest[index].is_some())
        .ok_or(DataIssue::NoData)?;
    if !now.is_finite() || now < last.seconds || now - last.seconds > config.max_age_s {
        return Err(DataIssue::Stale);
    }
    if let Measurement::Fault(fault) = last.measurement {
        return Err(DataIssue::Fault(fault));
    }
    if valid_raw(last.measurement).is_none() {
        return Err(DataIssue::InvalidRaw);
    }
    Ok(())
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
        return Unavailable(DataIssue::Disconnected);
    }
    if let Err(issue) = available(history, config, index, now) {
        return Unavailable(issue);
    }
    if config.validate().is_err() {
        return Pending(WaitReason::Settings);
    }
    let Some(zero) = config.cells[index].zero_raw.filter(|_| !zero_pending) else {
        return Pending(WaitReason::Zero);
    };
    let Ok(mean) = mean(history, config, index, now) else {
        return Pending(WaitReason::Averaging);
    };
    let cell = &config.cells[index];
    let kg = (mean.raw - zero) / cell.counts_per_kg;
    if !kg.is_finite() {
        return Pending(WaitReason::Settings);
    }
    Load {
        kg,
        nominal: !cell.calibrated,
    }
}

/// Uses only the latest contiguous healthy window. Silence longer than the
/// freshness limit also starts a new window, even without a firmware fault.
/// PC reception times cannot establish simultaneous ADC sampling.
pub fn mean(history: &History, config: &Config, index: usize, now: f64) -> Result<Mean, Error> {
    let error = |issue| Error::Reading {
        channel: Channel::ALL[index],
        issue,
    };
    available(history, config, index, now).map_err(error)?;
    let samples = &history.samples[index];
    let last = samples.back().ok_or_else(|| error(DataIssue::NoData))?;
    let (mut sum, mut count, mut newer) = (0.0, 0, last.seconds);
    for sample in samples.iter().rev() {
        if sample.seconds < now - config.average_s || newer - sample.seconds > config.max_age_s {
            break;
        }
        let Some(raw) = valid_raw(sample.measurement) else {
            break;
        };
        sum += raw;
        count += 1;
        newer = sample.seconds;
        if sample.break_before {
            break;
        }
    }
    if count < config.min_samples {
        return Err(error(DataIssue::InsufficientSamples));
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
