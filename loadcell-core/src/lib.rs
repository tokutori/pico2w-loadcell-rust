//! Platform-independent HX711 sample decoding and scale calibration.
//! This crate has no GPIO, USB, or time dependency.
#![no_std]

use vstd::prelude::*;

verus! {

/// Nominal HX711 counts per kilogram, derived from a 1.8 mV/V, 50 kg sensor
/// and gain 128. This is *not* a substitute for calibrating each sensor.
pub const NOMINAL_COUNTS_PER_KG: i32 = 77_309;

/// Convert the low 24 bits of an HX711 word to signed two's-complement.
/// Bits above bit 23 are ignored.
///
/// # Examples
/// ```
/// use loadcell_core::decode_24;
/// assert_eq!(decode_24(0x007f_ffff), 8_388_607);
/// assert_eq!(decode_24(0x0080_0000), -8_388_608);
/// assert_eq!(decode_24(0x00ff_ffff), -1);
/// ```
pub const fn decode_24(bits: u32) -> (value: i32)
    ensures
        -8_388_608 <= value <= 8_388_607,
        value as int == if bits & 0x0080_0000 == 0 {
            (bits & 0x00ff_ffff) as int
        } else {
            (bits & 0x00ff_ffff) as int - 16_777_216
        },
{
    let value = (((bits & 0x00ff_ffff) << 8) as i32) >> 8;
    proof {
        assert(-8_388_608i32 <= value && value <= 8_388_607i32) by (bit_vector);
        assert(value as i64 == if bits & 0x0080_0000u32 == 0 {
            (bits & 0x00ff_ffffu32) as i64
        } else {
            (bits & 0x00ff_ffffu32) as i64 - 16_777_216i64
        }) by (bit_vector);
    }
    value
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalibrationError {
    ZeroSensitivity,
}

/// Offset and signed span. `counts_per_kg` may be negative when the
/// electrical polarity or loading direction is reversed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Calibration {
    zero: i32,
    counts_per_kg: i32,
}

impl Calibration {
    pub const fn new(zero: i32, counts_per_kg: i32) -> (result: Result<Self, CalibrationError>)
        ensures
            match result {
                Ok(scale) => counts_per_kg != 0
                    && scale.zero == zero
                    && scale.counts_per_kg == counts_per_kg,
                Err(CalibrationError::ZeroSensitivity) => counts_per_kg == 0,
            },
    {
        if counts_per_kg == 0 {
            Err(CalibrationError::ZeroSensitivity)
        } else {
            Ok(Self {
                zero,
                counts_per_kg,
            })
        }
    }

    pub const fn zero(self) -> (zero: i32)
        ensures zero == self.zero,
    {
        self.zero
    }

    pub const fn counts_per_kg(self) -> (counts_per_kg: i32)
        ensures counts_per_kg == self.counts_per_kg,
    {
        self.counts_per_kg
    }

    /// Return the exact difference for every pair of signed 32-bit values.
    /// The result is within [-4_294_967_295, 4_294_967_295] and cannot overflow i64.
    pub fn delta(self, raw: i32) -> (delta: i64)
        ensures
            delta as int == raw as int - self.zero as int,
            -4_294_967_295 <= delta <= 4_294_967_295,
    {
        i64::from(raw) - i64::from(self.zero)
    }
}

} // verus!

impl Calibration {
    /// Return an approximate load in kg. Precision depends on calibration
    /// and the load cell's mechanical installation.
    pub fn kilograms(self, raw: i32) -> f32 {
        (self.delta(raw) as f32) / (self.counts_per_kg as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_extension_and_masking() {
        assert_eq!(decode_24(0), 0);
        assert_eq!(decode_24(0x007f_ffff), 8_388_607);
        assert_eq!(decode_24(0x0080_0000), -8_388_608);
        assert_eq!(decode_24(0x00ff_ffff), -1);
        assert_eq!(decode_24(0xffff_ffff), -1);
    }

    #[test]
    fn signed_sensitivity_and_tare() {
        let normal = Calibration::new(100, 20_000).unwrap();
        assert_eq!(normal.delta(40_100), 40_000);
        assert_eq!(normal.kilograms(40_100), 2.0);
        let reversed = Calibration::new(100, -20_000).unwrap();
        assert_eq!(reversed.kilograms(-39_900), 2.0);
    }

    #[test]
    fn rejects_zero_sensitivity() {
        assert_eq!(
            Calibration::new(0, 0),
            Err(CalibrationError::ZeroSensitivity)
        );
    }
}
