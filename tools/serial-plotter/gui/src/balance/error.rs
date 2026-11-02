//! Domain failures carry data; user-facing wording lives in Display.
use serial_plotter_core::{Channel, Fault};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataIssue {
    Disconnected,
    NoData,
    Stale,
    Gap,
    FirmwareTare,
    Fault(Fault),
    InvalidRaw,
    InsufficientSamples,
}

impl fmt::Display for DataIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Disconnected => "disconnected",
            Self::NoData => "no data",
            Self::Stale => "stale",
            Self::Gap => "data gap",
            Self::FirmwareTare => "firmware tare in progress",
            Self::Fault(Fault::Timeout) => "timeout",
            Self::Fault(Fault::Saturated) => "saturated",
            Self::Fault(Fault::Protocol) => "protocol error",
            Self::InvalidRaw => "invalid raw",
            Self::InsufficientSamples => "not enough fresh, uninterrupted samples",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitReason {
    Zero,
    Settings,
    Averaging,
}

impl fmt::Display for WaitReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Zero => "RX: zero needed",
            Self::Settings => "RX: check settings",
            Self::Averaging => "RX: averaging",
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    Version(u32),
    Assignment,
    LayoutFormat,
    Coordinates,
    Calibration(Channel),
    TraceWindow,
    Limits,
    TooFewSupports,
    Collinear,
    NoEnabledSensors,
    DisabledSensor(Channel),
    TimeSkew,
    ZeroMissing(Channel),
    NegativeLoad { channel: Channel, kg: f64 },
    LowTotal { kg: f64, minimum: f64 },
    Overflow,
    KnownMass,
    LoadChange,
    Reading { channel: Channel, issue: DataIssue },
    ZeroPending,
    Disconnected,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Version(v) => write!(f, "Unsupported settings version: {v}"),
            Self::Assignment => {
                f.write_str("Assign each LC exactly once to the four support positions")
            }
            Self::LayoutFormat => f.write_str(
                "Layout requires points + channels, or a complete legacy dimension layout",
            ),
            Self::Coordinates => f.write_str(
                "Measurement point coordinates must be finite and within +/- 1000000 mm",
            ),
            Self::Calibration(c) => write!(f, "{}: invalid zero or signed counts/kg", c.label()),
            Self::TraceWindow => f.write_str("CG time graph window must be 1 to 300 seconds"),
            Self::Limits => f.write_str("Invalid averaging, freshness or load limits"),
            Self::TooFewSupports => {
                f.write_str("Enable at least 3 non-collinear supports for 2D CG")
            }
            Self::Collinear => f.write_str("Supports must not lie on one line"),
            Self::DisabledSensor(c) => write!(
                f,
                "{} is disabled; enable all four sensors for the total load",
                c.label()
            ),
            Self::NoEnabledSensors => f.write_str("Enable the sensors supporting the object"),
            Self::TimeSkew => {
                f.write_str("Sensor reception times are too far apart; waiting for aligned data")
            }
            Self::ZeroMissing(c) => write!(f, "{}: capture platform-only zero first", c.label()),
            Self::NegativeLoad { channel, kg } => write!(
                f,
                "{}: negative load {kg:.3} kg; check polarity, zero and support contact",
                channel.label()
            ),
            Self::LowTotal { kg, minimum } => write!(
                f,
                "Total load {kg:.3} kg is below the {minimum:.3} kg limit"
            ),
            Self::Overflow => f.write_str("Calculation overflow; check calibration"),
            Self::KnownMass => f.write_str("Known mass must be positive and finite"),
            Self::LoadChange => {
                f.write_str("Load change is less than 1 ADC count; check load and zero")
            }
            Self::Reading { channel, issue } => write!(f, "{}: {issue}", channel.label()),
            Self::ZeroPending => f.write_str("PC zero pending: keep the empty platform still"),
            Self::Disconnected => f.write_str("Disconnected: connect to receive fresh readings."),
        }
    }
}
impl std::error::Error for Error {}
