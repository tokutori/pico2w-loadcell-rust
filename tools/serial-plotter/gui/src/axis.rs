//! Validated vertical bounds shared by automatic fitting and manual entry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Range {
    pub min: f64,
    pub max: f64,
}

impl Default for Range {
    fn default() -> Self {
        Self {
            min: -1.0,
            max: 1.0,
        }
    }
}

impl Range {
    pub fn parse(lower: &str, upper: &str) -> Result<Self, &'static str> {
        let min = lower.trim().parse().map_err(|_| "Enter numeric limits")?;
        let max = upper.trim().parse().map_err(|_| "Enter numeric limits")?;
        Self::new(min, max).ok_or("Limits must be finite; lower < upper")
    }

    pub fn new(min: f64, max: f64) -> Option<Self> {
        (min.is_finite() && max.is_finite() && min < max && (max - min).is_finite())
            .then_some(Self { min, max })
    }

    /// Fit every finite sample, with headroom on both sides. A constant signal
    /// gets a nonzero span without forcing the origin into the visible range.
    pub fn fit(values: impl Iterator<Item = f64>, minimum_span: f64) -> Option<Self> {
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        for value in values.filter(|value| value.is_finite()) {
            min = min.min(value);
            max = max.max(value);
        }
        if !min.is_finite() || !max.is_finite() {
            return None;
        }
        let padding = ((max - min) * 0.05)
            .max(minimum_span / 2.0)
            .max(min.abs().max(max.abs()) * f64::EPSILON * 8.0);
        Self::new(min - padding, max + padding).or_else(|| Self::new(min, max))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_samples_fit_with_room_above_and_below() {
        let range = Range::fit([-1_585_505.0, -155_495.0, -1.0].into_iter(), 1.0).unwrap();
        assert!(range.min < -1_585_505.0);
        assert!(range.max > -1.0);
        let positive = Range::fit([100.0, 110.0].into_iter(), 1.0).unwrap();
        assert!(positive.min > 0.0);
        assert!(positive.max > 110.0);
    }

    #[test]
    fn flat_and_empty_signals_have_defined_behavior() {
        let range = Range::fit([-155_495.0].into_iter(), 1.0).unwrap();
        assert!(range.min < -155_495.0 && range.max > -155_495.0);
        assert_eq!(Range::fit(std::iter::empty(), 1.0), None);
        assert_eq!(Range::fit([f64::NAN].into_iter(), 1.0), None);
    }

    #[test]
    fn manual_limits_accept_negatives_and_reject_invalid_spans() {
        assert_eq!(
            Range::parse(" -200000 ", "-100000"),
            Ok(Range {
                min: -200000.0,
                max: -100000.0
            })
        );
        for (lower, upper) in [
            ("", "1"),
            ("2", "1"),
            ("1", "1"),
            ("NaN", "1"),
            ("0", "inf"),
            ("-1e308", "1e308"),
        ] {
            assert!(Range::parse(lower, upper).is_err());
        }
    }
}
