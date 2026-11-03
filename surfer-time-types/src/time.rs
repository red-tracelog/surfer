use derive_more::Display;
use enum_iterator::Sequence;
use serde::{Deserialize, Serialize};

/// Time scale metadata for simulator timestamps.
///
/// `unit` describes the base unit and `multiplier` scales that unit.
/// For example, `unit = PicoSeconds` and `multiplier = Some(10)` means
/// one timestamp tick corresponds to 10 ps.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TimeScale {
    pub unit: TimeUnit,
    pub multiplier: Option<u32>,
}

impl TimeScale {
    /// Build a [`TimeScale`] from a simulator resolution expressed in femtoseconds.
    ///
    /// The mapping prefers the smallest allowed multiplier from `{1, 10, 100}`
    /// and the coarsest matching unit in the supported unit table.
    ///
    /// If `resolution_fs` is zero or does not match any representable combination,
    /// this falls back to `1 fs`.
    #[must_use]
    pub fn from_fs(resolution_fs: u64) -> Self {
        let resolution_fs = if resolution_fs > 0 { resolution_fs } else { 1 };

        let units = [
            (TimeUnit::Seconds, 1_000_000_000_000_000u64),
            (TimeUnit::MilliSeconds, 1_000_000_000_000u64),
            (TimeUnit::MicroSeconds, 1_000_000_000u64),
            (TimeUnit::NanoSeconds, 1_000_000u64),
            (TimeUnit::PicoSeconds, 1_000u64),
            (TimeUnit::FemtoSeconds, 1u64),
        ];

        for multiplier in [1u32, 10u32, 100u32] {
            for (unit, unit_in_fs) in units {
                if unit_in_fs
                    .checked_mul(u64::from(multiplier))
                    .is_some_and(|scaled| scaled == resolution_fs)
                {
                    return TimeScale {
                        unit,
                        multiplier: Some(multiplier),
                    };
                }
            }
        }

        TimeScale {
            unit: TimeUnit::FemtoSeconds,
            multiplier: Some(1),
        }
    }

    #[must_use]
    pub fn multiplier_digits(&self) -> u8 {
        match self.multiplier {
            Some(1) => 0,
            Some(10) => 1,
            Some(100) => 2,
            Some(multiplier) => multiplier.ilog10() as u8,
            None => 0,
        }
    }
}

/// What 1 unit of time represents when sending value changes.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Display, Eq, PartialEq, Sequence)]
pub enum TimeUnit {
    #[display("zs")]
    ZeptoSeconds,
    #[display("as")]
    AttoSeconds,
    #[display("fs")]
    FemtoSeconds,
    #[display("ps")]
    PicoSeconds,
    #[display("ns")]
    NanoSeconds,
    #[display("μs")]
    MicroSeconds,
    #[display("ms")]
    MilliSeconds,
    #[display("s")]
    Seconds,
    #[display("No unit")]
    None,
    #[display("Auto")]
    Auto,
}

impl TimeUnit {
    #[must_use]
    pub fn exponent(self) -> i8 {
        match self {
            TimeUnit::ZeptoSeconds => -21,
            TimeUnit::AttoSeconds => -18,
            TimeUnit::FemtoSeconds => -15,
            TimeUnit::PicoSeconds => -12,
            TimeUnit::NanoSeconds => -9,
            TimeUnit::MicroSeconds => -6,
            TimeUnit::MilliSeconds => -3,
            TimeUnit::Seconds => 0,
            TimeUnit::None => 0,
            TimeUnit::Auto => 0,
        }
    }

    #[must_use]
    pub fn to_fs(&self) -> f64 {
        match self {
            TimeUnit::ZeptoSeconds => 0.000_001,
            TimeUnit::AttoSeconds => 0.001,
            TimeUnit::FemtoSeconds => 1.0,
            TimeUnit::PicoSeconds => 1_000.0,
            TimeUnit::NanoSeconds => 1_000_000.0,
            TimeUnit::MicroSeconds => 1_000_000_000.0,
            TimeUnit::MilliSeconds => 1_000_000_000_000.0,
            TimeUnit::Seconds => 1_000_000_000_000_000.0,
            TimeUnit::None | TimeUnit::Auto => 1.0,
        }
    }

    #[must_use]
    pub fn from_exponent(exponent: i8) -> Option<Self> {
        match exponent {
            -21 => Some(TimeUnit::ZeptoSeconds),
            -18 => Some(TimeUnit::AttoSeconds),
            -15 => Some(TimeUnit::FemtoSeconds),
            -12 => Some(TimeUnit::PicoSeconds),
            -9 => Some(TimeUnit::NanoSeconds),
            -6 => Some(TimeUnit::MicroSeconds),
            -3 => Some(TimeUnit::MilliSeconds),
            0 => Some(TimeUnit::Seconds),
            _ => None,
        }
    }
}

#[cfg(feature = "wellen")]
impl From<wellen::TimescaleUnit> for TimeUnit {
    fn from(timescale: wellen::TimescaleUnit) -> Self {
        match timescale {
            wellen::TimescaleUnit::ZeptoSeconds => TimeUnit::ZeptoSeconds,
            wellen::TimescaleUnit::AttoSeconds => TimeUnit::AttoSeconds,
            wellen::TimescaleUnit::FemtoSeconds => TimeUnit::FemtoSeconds,
            wellen::TimescaleUnit::PicoSeconds => TimeUnit::PicoSeconds,
            wellen::TimescaleUnit::NanoSeconds => TimeUnit::NanoSeconds,
            wellen::TimescaleUnit::MicroSeconds => TimeUnit::MicroSeconds,
            wellen::TimescaleUnit::MilliSeconds => TimeUnit::MilliSeconds,
            wellen::TimescaleUnit::Seconds => TimeUnit::Seconds,
            wellen::TimescaleUnit::Unknown => TimeUnit::None,
        }
    }
}

#[cfg(feature = "wellen")]
impl From<&TimeScale> for wellen::Timescale {
    fn from(time_scale: &TimeScale) -> Self {
        let unit = match time_scale.unit {
            TimeUnit::ZeptoSeconds => wellen::TimescaleUnit::ZeptoSeconds,
            TimeUnit::AttoSeconds => wellen::TimescaleUnit::AttoSeconds,
            TimeUnit::FemtoSeconds => wellen::TimescaleUnit::FemtoSeconds,
            TimeUnit::PicoSeconds => wellen::TimescaleUnit::PicoSeconds,
            TimeUnit::NanoSeconds => wellen::TimescaleUnit::NanoSeconds,
            TimeUnit::MicroSeconds => wellen::TimescaleUnit::MicroSeconds,
            TimeUnit::MilliSeconds => wellen::TimescaleUnit::MilliSeconds,
            TimeUnit::Seconds => wellen::TimescaleUnit::Seconds,
            TimeUnit::None | TimeUnit::Auto => wellen::TimescaleUnit::Unknown,
        };

        wellen::Timescale::new(time_scale.multiplier.unwrap_or(1), unit)
    }
}

#[cfg(test)]
mod tests {
    use super::{TimeScale, TimeUnit};

    #[test]
    fn from_fs_maps_base_units_with_multiplier_one() {
        let cases = [
            (1u64, TimeUnit::FemtoSeconds),
            (1_000u64, TimeUnit::PicoSeconds),
            (1_000_000u64, TimeUnit::NanoSeconds),
            (1_000_000_000u64, TimeUnit::MicroSeconds),
            (1_000_000_000_000u64, TimeUnit::MilliSeconds),
            (1_000_000_000_000_000u64, TimeUnit::Seconds),
        ];

        for (resolution_fs, expected_unit) in cases {
            let scale = TimeScale::from_fs(resolution_fs);
            assert_eq!(scale.unit, expected_unit);
            assert_eq!(scale.multiplier, Some(1));
        }
    }

    #[test]
    fn from_fs_uses_smallest_allowed_multiplier() {
        let scale = TimeScale::from_fs(10_000);
        assert_eq!(scale.unit, TimeUnit::PicoSeconds);
        assert_eq!(scale.multiplier, Some(10));

        let scale = TimeScale::from_fs(100_000);
        assert_eq!(scale.unit, TimeUnit::PicoSeconds);
        assert_eq!(scale.multiplier, Some(100));
    }

    #[test]
    fn from_fs_zero_and_unmatched_values_fallback_to_1fs() {
        let scale = TimeScale::from_fs(0);
        assert_eq!(scale.unit, TimeUnit::FemtoSeconds);
        assert_eq!(scale.multiplier, Some(1));

        let scale = TimeScale::from_fs(2);
        assert_eq!(scale.unit, TimeUnit::FemtoSeconds);
        assert_eq!(scale.multiplier, Some(1));
    }
}
