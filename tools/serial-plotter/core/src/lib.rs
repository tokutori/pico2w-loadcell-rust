//! Allocation-free parsing of the firmware's LC1..LC4 text protocol.
#![no_std]

/// Physical channel in the four-channel firmware.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Lc1,
    Lc2,
    Lc3,
    Lc4,
}

impl Channel {
    pub const ALL: [Self; 4] = [Self::Lc1, Self::Lc2, Self::Lc3, Self::Lc4];

    pub const fn index(self) -> usize {
        match self {
            Self::Lc1 => 0,
            Self::Lc2 => 1,
            Self::Lc3 => 2,
            Self::Lc4 => 3,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Lc1 => "LC1",
            Self::Lc2 => "LC2",
            Self::Lc3 => "LC3",
            Self::Lc4 => "LC4",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    Timeout,
    Saturated,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Measurement {
    Sample { raw: i32, delta: i64, kg: f64 },
    Untared { raw: i32 },
    Fault(Fault),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    pub channel: Channel,
    pub measurement: Measurement,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    Raw,
    Delta,
    Kg,
}

impl Metric {
    pub const ALL: [Self; 3] = [Self::Raw, Self::Delta, Self::Kg];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Raw => "raw (ADC counts)",
            Self::Delta => "delta (ADC counts)",
            Self::Kg => "kg~ (nominal, uncalibrated)",
        }
    }
}

impl Measurement {
    /// Missing measurements remain missing; never substitute zero for faults.
    pub fn value(self, metric: Metric) -> Option<f64> {
        match (self, metric) {
            (Self::Sample { raw, .. } | Self::Untared { raw }, Metric::Raw) => Some(f64::from(raw)),
            (Self::Sample { delta, .. }, Metric::Delta) => Some(delta as f64),
            (Self::Sample { kg, .. }, Metric::Kg) => Some(kg),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    Channel,
    Format,
    Number,
}

/// Parse one complete firmware line in O(line length), without I/O or allocation.
///
/// Unrelated boot/terminal text returns `Ok(None)`. LC-prefixed malformed lines
/// return an error. Only the three documented firmware layouts are accepted;
/// non-finite weights, unknown channels and trailing tokens are rejected.
/// Values including `raw=-1` and zero are preserved without filtering.
///
/// ```
/// use serial_plotter_core::{parse_line, Metric};
/// let reading = parse_line("LC1 raw=-1 delta=141512 kg~=1.830").unwrap().unwrap();
/// assert_eq!(reading.measurement.value(Metric::Raw), Some(-1.0));
/// assert_eq!(parse_line("Tare complete."), Ok(None));
/// ```
pub fn parse_line(line: &str) -> Result<Option<Reading>, ParseError> {
    let mut fields = line.split_ascii_whitespace();
    let Some(first) = fields.next() else {
        return Ok(None);
    };
    let channel = match first {
        "LC1" => Channel::Lc1,
        "LC2" => Channel::Lc2,
        "LC3" => Channel::Lc3,
        "LC4" => Channel::Lc4,
        other if other.starts_with("LC") => return Err(ParseError::Channel),
        _ => return Ok(None),
    };
    let measurement = match (fields.next(), fields.next(), fields.next(), fields.next()) {
        (Some("status=timeout"), None, None, None) => Measurement::Fault(Fault::Timeout),
        (Some("status=saturated"), None, None, None) => Measurement::Fault(Fault::Saturated),
        (Some(raw), Some("status=no_tare"), None, None) => Measurement::Untared {
            raw: number(raw, "raw=")?,
        },
        (Some(raw), Some(delta), Some(kg), None) => {
            let kg: f64 = number(kg, "kg~=")?;
            if !kg.is_finite() {
                return Err(ParseError::Number);
            }
            Measurement::Sample {
                raw: number(raw, "raw=")?,
                delta: number(delta, "delta=")?,
                kg,
            }
        }
        _ => return Err(ParseError::Format),
    };
    Ok(Some(Reading {
        channel,
        measurement,
    }))
}

fn number<T: core::str::FromStr>(field: &str, prefix: &str) -> Result<T, ParseError> {
    field
        .strip_prefix(prefix)
        .ok_or(ParseError::Format)?
        .parse()
        .map_err(|_| ParseError::Number)
}

/// Result of feeding a byte into the bounded line framer.
#[derive(Debug, PartialEq)]
pub enum Frame<'a> {
    Pending,
    Line(&'a str),
    InvalidUtf8,
    TooLong,
}

/// LF-delimited framing across arbitrary serial read boundaries.
///
/// CRLF is accepted. Overlong input is discarded through the next LF, emitting
/// one `TooLong` event per line. Storage is bounded by N bytes, with no allocation.
pub struct Framer<const N: usize> {
    bytes: [u8; N],
    len: usize,
    discarding: bool,
}

impl<const N: usize> Default for Framer<N> {
    fn default() -> Self {
        Self {
            bytes: [0; N],
            len: 0,
            discarding: false,
        }
    }
}

impl<const N: usize> Framer<N> {
    /// Incomplete line for a live monitor; not yet eligible for parsing.
    pub fn pending_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    pub fn push(&mut self, byte: u8) -> Frame<'_> {
        if byte == b'\n' {
            let len = self.len;
            self.len = 0;
            if self.discarding {
                self.discarding = false;
                return Frame::Pending;
            }
            return match core::str::from_utf8(&self.bytes[..len]) {
                Ok(line) => Frame::Line(line.trim_end_matches('\r')),
                Err(_) => Frame::InvalidUtf8,
            };
        }
        if self.discarding {
            return Frame::Pending;
        }
        if self.len == N {
            self.discarding = true;
            self.len = 0;
            return Frame::TooLong;
        }
        self.bytes[self.len] = byte;
        self.len += 1;
        Frame::Pending
    }
}
