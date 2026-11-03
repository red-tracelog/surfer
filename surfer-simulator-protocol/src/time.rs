//! Nonnegative simulator ticks. The hierarchy exponent defines the tick size.
use serde::{Deserialize, Serialize};

/// A relative amount of simulation time, distinct from an absolute timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SimulationDuration(u64);

/// An absolute position on the simulation timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SimulationTimestamp(u64);

/// A run limit in simulator ticks. Absence of a limit means run to completion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunTime {
    Relative(SimulationDuration),
    Absolute(SimulationTimestamp),
}

impl RunTime {
    /// Resolve to a backend delay. Reject past deadlines and out-of-range times.
    /// A deadline equal to `now` resolves to zero (finish the current timestep).
    pub fn checked_delay(
        self,
        now: SimulationTimestamp,
        max_ticks: u64,
    ) -> Option<SimulationDuration> {
        match self {
            Self::Relative(duration) => now.checked_advance(duration, max_ticks).map(|_| duration),
            Self::Absolute(deadline) => {
                if deadline.ticks() > max_ticks {
                    return None;
                }
                deadline
                    .ticks()
                    .checked_sub(now.ticks())
                    .map(SimulationDuration::from_ticks)
            }
        }
    }
}

impl SimulationDuration {
    pub const fn from_ticks(ticks: u64) -> Self {
        Self(ticks)
    }

    pub const fn ticks(self) -> u64 {
        self.0
    }

    /// Checked conversion for backends with signed 64-bit time.
    pub fn to_i64(self) -> Result<i64, std::num::TryFromIntError> {
        i64::try_from(self.0)
    }
}

impl SimulationTimestamp {
    pub const fn from_ticks(ticks: u64) -> Self {
        Self(ticks)
    }

    pub const fn ticks(self) -> u64 {
        self.0
    }

    /// Validate the absolute deadline before changing simulator callbacks.
    pub fn checked_advance(self, duration: SimulationDuration, max_ticks: u64) -> Option<Self> {
        self.0
            .checked_add(duration.0)
            .filter(|deadline| *deadline <= max_ticks)
            .map(Self)
    }
}

impl TryFrom<i64> for SimulationTimestamp {
    type Error = std::num::TryFromIntError;

    fn try_from(ticks: i64) -> Result<Self, Self::Error> {
        u64::try_from(ticks).map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_limits_resolve_to_backend_delays() {
        let now = SimulationTimestamp::from_ticks(10);
        let relative = |ticks| RunTime::Relative(SimulationDuration::from_ticks(ticks));
        let absolute = |ticks| RunTime::Absolute(SimulationTimestamp::from_ticks(ticks));
        for max in [i64::MAX as u64, u64::MAX] {
            assert_eq!(
                relative(5).checked_delay(now, max),
                absolute(15).checked_delay(now, max)
            );
            assert_eq!(
                absolute(10).checked_delay(now, max),
                Some(SimulationDuration::from_ticks(0))
            );
            assert_eq!(
                relative(0).checked_delay(now, max),
                Some(SimulationDuration::from_ticks(0))
            );
            assert!(absolute(9).checked_delay(now, max).is_none());
            assert_eq!(
                absolute(max).checked_delay(now, max),
                Some(SimulationDuration::from_ticks(max - 10))
            );
            assert!(relative(max).checked_delay(now, max).is_none());
        }
        assert!(
            absolute(i64::MAX as u64 + 1)
                .checked_delay(now, i64::MAX as u64)
                .is_none()
        );
    }

    #[test]
    fn signed_backend_boundaries() {
        assert!(SimulationTimestamp::try_from(-1).is_err());
        assert!(SimulationTimestamp::try_from(i64::MIN).is_err());
        assert_eq!(SimulationTimestamp::try_from(0).unwrap().ticks(), 0);
        assert_eq!(
            SimulationDuration::from_ticks(i64::MAX as u64).to_i64(),
            Ok(i64::MAX)
        );
        assert!(
            SimulationDuration::from_ticks(i64::MAX as u64 + 1)
                .to_i64()
                .is_err()
        );
    }

    #[test]
    fn deadlines_respect_backend_limits() {
        let zero = SimulationDuration::from_ticks(0);
        let one = SimulationDuration::from_ticks(1);
        for max in [i64::MAX as u64, u64::MAX] {
            let end = SimulationTimestamp::from_ticks(max);
            assert_eq!(end.checked_advance(zero, max), Some(end));
            assert!(end.checked_advance(one, max).is_none());
            assert_eq!(
                SimulationTimestamp::from_ticks(max - 1).checked_advance(one, max),
                Some(end)
            );
        }
        assert_eq!(
            SimulationTimestamp::from_ticks(0)
                .checked_advance(SimulationDuration::from_ticks(u64::MAX), u64::MAX)
                .unwrap()
                .ticks(),
            u64::MAX
        );
    }
}
