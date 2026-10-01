//! Platform-independent deterministic simulation primitives.
//!
//! This crate deliberately has no windowing, rendering, filesystem, wall-clock,
//! or networking dependencies. Keep simulation code portable and testable.

/// A fixed-rate simulation clock driven by integer nanoseconds.
///
/// Time is accumulated as `nanoseconds * ticks_per_second`, avoiding floating
/// point drift in the clock itself. Feed elapsed time from the host loop; never
/// read the system clock from deterministic simulation code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixedClock {
    ticks_per_second: u32,
    tick: u64,
    accumulator: u128,
}

impl FixedClock {
    /// Creates a clock. A rate of zero is invalid.
    ///
    /// # Errors
    ///
    /// Returns [`ClockError::ZeroRate`] when `ticks_per_second` is zero.
    pub fn new(ticks_per_second: u32) -> Result<Self, ClockError> {
        if ticks_per_second == 0 {
            return Err(ClockError::ZeroRate);
        }
        Ok(Self {
            ticks_per_second,
            tick: 0,
            accumulator: 0,
        })
    }

    /// Current completed simulation tick.
    #[must_use]
    pub const fn tick(&self) -> u64 {
        self.tick
    }

    /// Advances the clock by elapsed wall time and returns the number of whole
    /// simulation steps now due. Call the simulation exactly that many times.
    ///
    /// The caller controls catch-up policy. That policy is intentionally not
    /// hidden in this primitive, so headless tests and interactive games can
    /// make different, explicit choices.
    ///
    /// # Errors
    ///
    /// Returns [`ClockError::TooManySteps`] when one advance would exceed
    /// `u32::MAX` due steps, or [`ClockError::TickOverflow`] if the tick count
    /// cannot be represented. On error, the clock remains unchanged.
    pub fn advance_ns(&mut self, elapsed_ns: u64) -> Result<u32, ClockError> {
        let accumulated =
            self.accumulator + u128::from(elapsed_ns) * u128::from(self.ticks_per_second);
        let due =
            u32::try_from(accumulated / 1_000_000_000).map_err(|_| ClockError::TooManySteps)?;
        let tick = self
            .tick
            .checked_add(u64::from(due))
            .ok_or(ClockError::TickOverflow)?;
        self.accumulator = accumulated % 1_000_000_000;
        self.tick = tick;
        Ok(due)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockError {
    ZeroRate,
    TooManySteps,
    TickOverflow,
}

impl std::fmt::Display for ClockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroRate => f.write_str("simulation tick rate must be greater than zero"),
            Self::TooManySteps => f.write_str("elapsed time produces too many simulation steps"),
            Self::TickOverflow => f.write_str("simulation tick counter overflowed"),
        }
    }
}

impl std::error::Error for ClockError {}

#[cfg(test)]
mod tests {
    use super::{ClockError, FixedClock};

    #[test]
    fn rejects_zero_rate() {
        assert_eq!(FixedClock::new(0), Err(ClockError::ZeroRate));
    }

    #[test]
    fn one_second_at_sixty_hz_is_sixty_ticks() {
        let mut clock = FixedClock::new(60).unwrap();
        assert_eq!(clock.advance_ns(1_000_000_000), Ok(60));
        assert_eq!(clock.tick(), 60);
    }

    #[test]
    fn fractional_steps_accumulate_without_drift() {
        let mut clock = FixedClock::new(60).unwrap();
        let mut steps = 0;
        for _ in 0..60 {
            steps += clock.advance_ns(16_666_667).unwrap();
        }
        assert_eq!(steps, 60);
        assert_eq!(clock.tick(), 60);
    }

    #[test]
    fn rejected_large_advance_does_not_mutate_clock() {
        let mut clock = FixedClock::new(60).unwrap();
        let before = clock.clone();
        assert_eq!(clock.advance_ns(u64::MAX), Err(ClockError::TooManySteps));
        assert_eq!(clock, before);
    }
}
