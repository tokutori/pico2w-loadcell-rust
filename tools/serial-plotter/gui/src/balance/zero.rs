//! Deterministic auto-zero lifecycle. Capturing offsets is the caller's action.
use super::Config;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Requirements {
    enabled: [bool; 4],
    average: f64,
    samples: usize,
    age: f64,
    skew: f64,
}
impl From<&Config> for Requirements {
    fn from(c: &Config) -> Self {
        Self {
            enabled: c.cells.each_ref().map(|c| c.enabled),
            average: c.average_s,
            samples: c.min_samples,
            age: c.max_age_s,
            skew: c.max_skew_s,
        }
    }
}

#[derive(Default)]
pub enum AutoZero {
    #[default]
    Inactive,
    Waiting {
        since: Option<f64>,
        requirements: Requirements,
    },
}
impl AutoZero {
    pub fn start(config: &Config) -> Self {
        if config.auto_zero_on_connect {
            Self::Waiting {
                since: None,
                requirements: config.into(),
            }
        } else {
            Self::Inactive
        }
    }
    pub fn pending(&self) -> bool {
        matches!(self, Self::Waiting { .. })
    }
    pub fn interrupt_window(&mut self) {
        if let Self::Waiting { since, .. } = self {
            *since = None;
        }
    }
    /// A setting change or invalid data resets the continuous healthy interval.
    /// Enabling the preference alone never starts an unrequested mid-load tare.
    pub fn observe(&mut self, config: &Config, now: f64, healthy: bool) -> bool {
        if !config.auto_zero_on_connect {
            *self = Self::Inactive;
        }
        let Self::Waiting {
            since,
            requirements,
        } = self
        else {
            return false;
        };
        let current = Requirements::from(config);
        if *requirements != current {
            *since = None;
            *requirements = current;
        }
        if !healthy || !now.is_finite() {
            *since = None;
            return false;
        }
        if since.is_some_and(|start| now < start) {
            *since = None;
        }
        let start = *since.get_or_insert(now);
        now - start >= config.average_s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn changed_requirements_and_invalid_intervals_restart_waiting() {
        let mut config = Config::default();
        let mut zero = AutoZero::start(&config);
        assert!(!zero.observe(&config, 1.0, true));
        config.cells[3].enabled = false;
        assert!(!zero.observe(&config, 1.5, true));
        assert!(!zero.observe(&config, 1.9, false));
        assert!(!zero.observe(&config, 2.0, true));
        assert!(zero.observe(&config, 2.5, true));
        config.auto_zero_on_connect = false;
        assert!(!zero.observe(&config, 3.0, true));
        config.auto_zero_on_connect = true;
        assert!(!zero.observe(&config, 4.0, true));
        assert!(!zero.pending());
    }
}
