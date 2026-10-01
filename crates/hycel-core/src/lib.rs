//! Platform-independent deterministic simulation primitives.
//!
//! This crate deliberately has no windowing, rendering, filesystem, wall-clock,
//! or networking dependencies. The host supplies elapsed time and input; the
//! simulation never reads ambient time itself.

mod math;
mod replay;
mod schedule;
mod state_hash;
mod world;

pub use math::{Angle, MathError, SimScalar, Transform2D, Vec2};
pub use replay::{
    MAX_ACTIONS_PER_FRAME, MAX_REPLAY_FRAMES, MAX_REPLAY_JSON_BYTES, REPLAY_SCHEMA_VERSION, Replay,
    ReplayError, ReplayHeader,
};
pub use schedule::{
    DeterministicRng, InputFrame, RngError, Schedule, ScheduleError, ScheduledEvent, SystemError,
    SystemId, TickContext,
};
pub use state_hash::{CanonicalState, CanonicalWriter, StateHash};
pub use world::{ComponentStorage, EntityId, World, WorldError};

const NANOS_PER_SECOND: u128 = 1_000_000_000;
const TIME_SCALE_ONE: u64 = 1_u64 << 32;

/// Initial simulation rate used by interactive Hycel projects.
pub const DEFAULT_TICKS_PER_SECOND: u32 = 60;
/// Maximum simulation steps performed for one host-frame time sample.
pub const DEFAULT_MAX_CATCH_UP_STEPS: u32 = 8;

/// A deterministic non-negative time multiplier represented as unsigned Q32.32.
///
/// The fixed denominator means the clock can retain sub-tick time while the
/// multiplier changes. Use [`TimeScale::from_ratio`] rather than floating point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeScale(u128);

impl TimeScale {
    /// No simulation time passes. Any fractional tick already accumulated is
    /// preserved until time is unpaused.
    pub const PAUSED: Self = Self(0);
    /// Normal simulation speed.
    pub const NORMAL: Self = Self(1_u128 << 32);

    /// Creates a scale from a non-negative ratio. The value is truncated to
    /// Q32.32 precision. With `u32` inputs, positive ratios remain nonzero, but
    /// most ratios are approximated to the nearest representable value.
    ///
    /// # Errors
    ///
    /// Returns [`TimeScaleError::ZeroDenominator`] when `denominator` is zero.
    pub fn from_ratio(numerator: u32, denominator: u32) -> Result<Self, TimeScaleError> {
        if denominator == 0 {
            return Err(TimeScaleError::ZeroDenominator);
        }
        let raw = (u128::from(numerator) * u128::from(TIME_SCALE_ONE)) / u128::from(denominator);
        Ok(Self(raw))
    }
}

/// Result of applying one host-frame time sample under a bounded catch-up policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameAdvance {
    /// Whole simulation steps the caller must execute.
    pub steps: u32,
    /// Whole steps' worth of wall-time debt discarded because the catch-up cap
    /// was reached. Simulation tick IDs are not skipped.
    pub dropped_ticks: u128,
}

/// A fixed-rate simulation clock driven by host-supplied integer nanoseconds.
///
/// Time is accumulated as `nanoseconds * ticks_per_second * Q32.32 scale`,
/// avoiding floating-point drift. The host owns wall-clock sampling, pause, and
/// time-scale choices; simulation code receives only integer ticks.
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

    /// Current logical simulation tick. Catch-up-dropped wall time does not
    /// advance this counter.
    #[must_use]
    pub const fn tick(&self) -> u64 {
        self.tick
    }

    /// Advances without a catch-up cap, returning all due steps.
    ///
    /// This primitive is useful for headless execution. Interactive loops should
    /// use [`Self::advance_frame`] to bound work and explicitly account for
    /// dropped wall-time debt.
    ///
    /// # Errors
    ///
    /// Returns [`ClockError::TooManySteps`] if one advance exceeds `u32::MAX`
    /// steps, or [`ClockError::TickOverflow`] if the logical tick counter would
    /// overflow. On error, the clock remains unchanged.
    pub fn advance_ns(&mut self, elapsed_ns: u64) -> Result<u32, ClockError> {
        let accumulated = self.accumulate(elapsed_ns, TimeScale::NORMAL)?;
        let due =
            u32::try_from(accumulated / Self::threshold()).map_err(|_| ClockError::TooManySteps)?;
        let tick = self
            .tick
            .checked_add(u64::from(due))
            .ok_or(ClockError::TickOverflow)?;

        self.accumulator = accumulated % Self::threshold();
        self.tick = tick;
        Ok(due)
    }

    /// Advances under an explicit bounded catch-up policy and time scale.
    ///
    /// Any whole steps beyond `max_steps` are dropped as wall-time debt. The
    /// clock executes at most `max_steps`, reports the number dropped, and keeps
    /// the fractional remainder. This slows simulation under sustained overload
    /// rather than skipping logical ticks. `TimeScale::PAUSED` adds no elapsed
    /// simulation time and preserves the fractional remainder for resumption.
    ///
    /// # Errors
    ///
    /// Returns [`ClockError::ZeroCatchUpLimit`] for a zero `max_steps`,
    /// [`ClockError::TickOverflow`] if the logical tick counter would overflow,
    /// or [`ClockError::ArithmeticOverflow`] if the elapsed-time calculation
    /// cannot be represented. On error, the clock remains unchanged.
    pub fn advance_frame(
        &mut self,
        elapsed_ns: u64,
        scale: TimeScale,
        max_steps: u32,
    ) -> Result<FrameAdvance, ClockError> {
        if max_steps == 0 {
            return Err(ClockError::ZeroCatchUpLimit);
        }

        let accumulated = self.accumulate(elapsed_ns, scale)?;
        let threshold = Self::threshold();
        let due = accumulated / threshold;
        let steps =
            u32::try_from(due.min(u128::from(max_steps))).map_err(|_| ClockError::TooManySteps)?;
        let dropped_ticks = due - u128::from(steps);
        let tick = self
            .tick
            .checked_add(u64::from(steps))
            .ok_or(ClockError::TickOverflow)?;

        self.accumulator = accumulated % threshold;
        self.tick = tick;
        Ok(FrameAdvance {
            steps,
            dropped_ticks,
        })
    }

    fn threshold() -> u128 {
        NANOS_PER_SECOND * u128::from(TIME_SCALE_ONE)
    }

    fn accumulate(&self, elapsed_ns: u64, scale: TimeScale) -> Result<u128, ClockError> {
        let elapsed_units = u128::from(elapsed_ns)
            .checked_mul(u128::from(self.ticks_per_second))
            .and_then(|units| units.checked_mul(scale.0))
            .ok_or(ClockError::ArithmeticOverflow)?;
        self.accumulator
            .checked_add(elapsed_units)
            .ok_or(ClockError::ArithmeticOverflow)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeScaleError {
    ZeroDenominator,
}

impl std::fmt::Display for TimeScaleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroDenominator => {
                f.write_str("time-scale denominator must be greater than zero")
            }
        }
    }
}

impl std::error::Error for TimeScaleError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockError {
    ZeroRate,
    ZeroCatchUpLimit,
    TooManySteps,
    TickOverflow,
    ArithmeticOverflow,
}

impl std::fmt::Display for ClockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroRate => f.write_str("simulation tick rate must be greater than zero"),
            Self::ZeroCatchUpLimit => f.write_str("catch-up step limit must be greater than zero"),
            Self::TooManySteps => f.write_str("elapsed time produces too many simulation steps"),
            Self::TickOverflow => f.write_str("simulation tick counter overflowed"),
            Self::ArithmeticOverflow => f.write_str("elapsed-time calculation overflowed"),
        }
    }
}

impl std::error::Error for ClockError {}

#[cfg(test)]
mod tests {
    use super::{
        ClockError, DEFAULT_MAX_CATCH_UP_STEPS, DEFAULT_TICKS_PER_SECOND, FixedClock, TimeScale,
        TimeScaleError,
    };

    #[test]
    fn rejects_zero_rate() {
        assert_eq!(FixedClock::new(0), Err(ClockError::ZeroRate));
    }

    #[test]
    fn default_simulation_policy_is_sixty_hz_and_eight_catch_up_steps() {
        let mut clock = FixedClock::new(DEFAULT_TICKS_PER_SECOND).unwrap();
        let advance = clock
            .advance_frame(1_000_000_000, TimeScale::NORMAL, DEFAULT_MAX_CATCH_UP_STEPS)
            .unwrap();
        assert_eq!(advance.steps, 8);
        assert_eq!(advance.dropped_ticks, 52);
        assert_eq!(clock.tick(), 8);
    }

    #[test]
    fn one_second_at_sixty_hz_is_sixty_ticks_without_a_catch_up_cap() {
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
    fn half_speed_is_exact_and_preserves_fractional_time() {
        let half = TimeScale::from_ratio(1, 2).unwrap();
        let mut clock = FixedClock::new(60).unwrap();
        let mut steps = 0;
        for _ in 0..240 {
            steps += clock.advance_frame(8_333_334, half, 8).unwrap().steps;
        }
        assert_eq!(steps, 60);
        assert_eq!(clock.tick(), 60);
    }

    #[test]
    fn changing_time_scale_preserves_fractional_tick() {
        let half = TimeScale::from_ratio(1, 2).unwrap();
        let mut clock = FixedClock::new(60).unwrap();
        assert_eq!(
            clock
                .advance_frame(8_333_333, TimeScale::NORMAL, 8)
                .unwrap()
                .steps,
            0
        );
        assert_eq!(clock.advance_frame(16_666_667, half, 8).unwrap().steps, 0);
        assert_eq!(clock.advance_ns(1), Ok(1));
        assert_eq!(clock.tick(), 1);
    }

    #[test]
    fn pause_preserves_fractional_tick_and_does_not_accumulate_paused_time() {
        let mut clock = FixedClock::new(60).unwrap();
        assert_eq!(clock.advance_ns(8_000_000), Ok(0));
        assert_eq!(
            clock.advance_frame(u64::MAX, TimeScale::PAUSED, 8),
            Ok(super::FrameAdvance {
                steps: 0,
                dropped_ticks: 0
            })
        );
        assert_eq!(clock.advance_ns(8_666_667), Ok(1));
        assert_eq!(clock.tick(), 1);
    }

    #[test]
    fn catch_up_limit_discards_excess_whole_debt_without_skipping_tick_ids() {
        let mut clock = FixedClock::new(60).unwrap();
        let advance = clock
            .advance_frame(200_000_000, TimeScale::NORMAL, 4)
            .unwrap();
        assert_eq!(advance.steps, 4);
        assert_eq!(advance.dropped_ticks, 8);
        assert_eq!(clock.tick(), 4);
        assert_eq!(clock.advance_ns(16_666_667), Ok(1));
        assert_eq!(clock.tick(), 5);
    }

    #[test]
    fn zero_time_scale_denominator_is_rejected() {
        assert_eq!(
            TimeScale::from_ratio(1, 0),
            Err(TimeScaleError::ZeroDenominator)
        );
    }

    #[test]
    fn zero_catch_up_limit_is_rejected_without_mutating_clock() {
        let mut clock = FixedClock::new(60).unwrap();
        let before = clock.clone();
        assert_eq!(
            clock.advance_frame(100_000_000, TimeScale::NORMAL, 0),
            Err(ClockError::ZeroCatchUpLimit)
        );
        assert_eq!(clock, before);
    }

    #[test]
    fn rejected_large_advance_does_not_mutate_clock() {
        let mut clock = FixedClock::new(60).unwrap();
        let before = clock.clone();
        assert_eq!(clock.advance_ns(u64::MAX), Err(ClockError::TooManySteps));
        assert_eq!(clock, before);
    }

    #[test]
    fn tick_overflow_does_not_mutate_clock() {
        let mut clock = FixedClock::new(60).unwrap();
        clock.tick = u64::MAX;
        let before = clock.clone();
        assert_eq!(clock.advance_ns(16_666_667), Err(ClockError::TickOverflow));
        assert_eq!(clock, before);
    }
}
