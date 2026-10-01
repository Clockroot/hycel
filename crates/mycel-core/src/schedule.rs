//! Single-threaded ordered simulation schedule, tick inputs, RNG, and events.
//!
//! Systems are registered with explicit order keys and execute serially in
//! `(order, system ID)` order. Each system has a deterministic RNG stream derived
//! from the schedule seed/base stream and its ID. All authoritative state must
//! live in the cloned state passed to a system; callbacks must not commit external side
//! effects because failed ticks restore the state, RNG, and event queue.

use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::{CanonicalState, CanonicalWriter};

/// Stable numeric identifier for one scheduled system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SystemId(u32);

impl SystemId {
    /// Creates a system ID. IDs must be unique within a schedule.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Numeric system ID used as the deterministic tie-breaker for equal order.
    #[must_use]
    pub const fn value(self) -> u32 {
        self.0
    }
}

/// One tick-indexed snapshot of digital and analog game input.
///
/// Numeric action IDs are deliberately generic; named/rebindable player actions
/// are defined at a higher layer. Missing buttons are false and missing axes are
/// zero. Axis values use the full signed `i16` range.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputFrame {
    tick: u64,
    #[serde(deserialize_with = "deserialize_unique_map")]
    buttons: BTreeMap<u16, bool>,
    #[serde(deserialize_with = "deserialize_unique_map")]
    axes: BTreeMap<u16, i16>,
}

fn deserialize_unique_map<'de, D, K, V>(deserializer: D) -> Result<BTreeMap<K, V>, D::Error>
where
    D: Deserializer<'de>,
    K: Deserialize<'de> + Ord,
    V: Deserialize<'de>,
{
    struct UniqueMapVisitor<K, V>(PhantomData<(K, V)>);

    impl<'de, K, V> Visitor<'de> for UniqueMapVisitor<K, V>
    where
        K: Deserialize<'de> + Ord,
        V: Deserialize<'de>,
    {
        type Value = BTreeMap<K, V>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a map with unique keys")
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            let mut values = BTreeMap::new();
            while let Some((key, value)) = map.next_entry()? {
                if values.contains_key(&key) {
                    return Err(de::Error::custom("duplicate action ID"));
                }
                values.insert(key, value);
            }
            Ok(values)
        }
    }

    deserializer.deserialize_map(UniqueMapVisitor(PhantomData))
}

impl CanonicalState for InputFrame {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.write_u64(self.tick);
        writer.write_sequence_len(self.buttons.len());
        for (action, pressed) in &self.buttons {
            writer.write_u16(*action);
            writer.write_bool(*pressed);
        }
        writer.write_sequence_len(self.axes.len());
        for (action, value) in &self.axes {
            writer.write_u16(*action);
            writer.write_i16(*value);
        }
    }
}

impl InputFrame {
    /// Creates an empty input snapshot for `tick`.
    #[must_use]
    pub fn new(tick: u64) -> Self {
        Self {
            tick,
            buttons: BTreeMap::new(),
            axes: BTreeMap::new(),
        }
    }

    /// Tick number this snapshot applies to.
    #[must_use]
    pub const fn tick(&self) -> u64 {
        self.tick
    }

    /// Sets a digital action state.
    pub fn set_button(&mut self, action_id: u16, pressed: bool) {
        self.buttons.insert(action_id, pressed);
    }

    pub(crate) fn action_count(&self) -> usize {
        self.buttons.len() + self.axes.len()
    }

    /// Whether the digital action is pressed; absent actions are not pressed.
    #[must_use]
    pub fn button(&self, action_id: u16) -> bool {
        self.buttons.get(&action_id).copied().unwrap_or(false)
    }

    /// Sets a signed analog action value.
    pub fn set_axis(&mut self, action_id: u16, value: i16) {
        self.axes.insert(action_id, value);
    }

    /// Analog action value; absent actions are zero.
    #[must_use]
    pub fn axis(&self, action_id: u16) -> i16 {
        self.axes.get(&action_id).copied().unwrap_or(0)
    }
}

/// Seeded PCG-XSH-RR 64/32 generator for deterministic simulation draws.
///
/// The algorithm, seed, and stream fully determine the output sequence. Use
/// separate stream values for independent systems; do not use a host RNG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeterministicRng {
    state: u64,
    increment: u64,
}

impl DeterministicRng {
    /// Initializes a generator from a seed and stream selector.
    #[must_use]
    pub fn new(seed: u64, stream: u64) -> Self {
        let mut rng = Self {
            state: 0,
            increment: stream.wrapping_shl(1) | 1,
        };
        let _ = rng.next_u32();
        rng.state = rng.state.wrapping_add(seed);
        let _ = rng.next_u32();
        rng
    }

    /// Returns the next 32 pseudorandom bits.
    #[must_use]
    pub fn next_u32(&mut self) -> u32 {
        const MULTIPLIER: u64 = 6_364_136_223_846_793_005;
        let old_state = self.state;
        self.state = old_state
            .wrapping_mul(MULTIPLIER)
            .wrapping_add(self.increment);
        let shifted = ((old_state >> 18) ^ old_state) >> 27;
        let bytes = shifted.to_le_bytes();
        let value = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let rotation = u32::from(old_state.to_le_bytes()[7] >> 3);
        value.rotate_right(rotation)
    }

    /// Returns the next 64 pseudorandom bits, high word first.
    #[must_use]
    pub fn next_u64(&mut self) -> u64 {
        (u64::from(self.next_u32()) << 32) | u64::from(self.next_u32())
    }

    /// Returns an unbiased value in `0..upper_exclusive`.
    ///
    /// # Errors
    ///
    /// Returns [`RngError::EmptyRange`] when `upper_exclusive` is zero.
    pub fn below(&mut self, upper_exclusive: u32) -> Result<u32, RngError> {
        if upper_exclusive == 0 {
            return Err(RngError::EmptyRange);
        }
        let threshold = upper_exclusive.wrapping_neg() % upper_exclusive;
        loop {
            let value = self.next_u32();
            if value >= threshold {
                return Ok(value % upper_exclusive);
            }
        }
    }
}

/// Event produced by a system and delivered in a future tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledEvent<E> {
    delivery_tick: u64,
    producer: SystemId,
    sequence: u64,
    payload: E,
}

impl<E> ScheduledEvent<E> {
    /// Tick at which this event is delivered to systems.
    #[must_use]
    pub const fn delivery_tick(&self) -> u64 {
        self.delivery_tick
    }

    /// System that emitted this event.
    #[must_use]
    pub const fn producer(&self) -> SystemId {
        self.producer
    }

    /// Per-schedule emission sequence, used as the final ordering key.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Event payload.
    #[must_use]
    pub const fn payload(&self) -> &E {
        &self.payload
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EventQueue<E> {
    next_sequence: u64,
    pending: Vec<ScheduledEvent<E>>,
}

impl<E> Default for EventQueue<E> {
    fn default() -> Self {
        Self {
            next_sequence: 0,
            pending: Vec::new(),
        }
    }
}

impl<E> EventQueue<E> {
    fn emit(
        &mut self,
        delivery_tick: u64,
        producer: SystemId,
        payload: E,
    ) -> Result<(), SystemError> {
        let next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(SystemError::EventSequenceOverflow)?;
        self.pending.push(ScheduledEvent {
            delivery_tick,
            producer,
            sequence: self.next_sequence,
            payload,
        });
        self.next_sequence = next_sequence;
        Ok(())
    }

    fn take_due(&mut self, tick: u64) -> Vec<ScheduledEvent<E>> {
        let mut due = Vec::new();
        let mut pending = Vec::with_capacity(self.pending.len());
        for event in self.pending.drain(..) {
            if event.delivery_tick == tick {
                due.push(event);
            } else {
                pending.push(event);
            }
        }
        self.pending = pending;
        due.sort_by_key(|event| (event.delivery_tick, event.producer, event.sequence));
        due
    }
}

/// Mutable per-system view for one deterministic tick.
pub struct TickContext<'a, E> {
    tick: u64,
    producer: SystemId,
    input: &'a InputFrame,
    events: &'a [ScheduledEvent<E>],
    rng: &'a mut DeterministicRng,
    outgoing: &'a mut EventQueue<E>,
}

impl<E> TickContext<'_, E> {
    /// Current simulation tick.
    #[must_use]
    pub const fn tick(&self) -> u64 {
        self.tick
    }

    /// Input snapshot assigned to this tick.
    #[must_use]
    pub const fn input(&self) -> &InputFrame {
        self.input
    }

    /// Events emitted in earlier ticks and delivered now, in canonical order.
    #[must_use]
    pub fn events(&self) -> &[ScheduledEvent<E>] {
        self.events
    }

    /// Draws from this schedule's deterministic RNG stream.
    pub fn random_u32(&mut self) -> u32 {
        self.rng.next_u32()
    }

    /// Emits an event for the next tick. Same-tick recursive event delivery is
    /// intentionally prohibited; all systems observe the same current event set.
    ///
    /// # Errors
    ///
    /// Returns [`SystemError::EventSequenceOverflow`] if the schedule has
    /// exhausted its event sequence counter.
    pub fn emit(&mut self, payload: E) -> Result<(), SystemError> {
        let delivery_tick = self
            .tick
            .checked_add(1)
            .ok_or(SystemError::EventSequenceOverflow)?;
        self.outgoing.emit(delivery_tick, self.producer, payload)
    }
}

type SystemFunction<State, Event> =
    dyn for<'a> Fn(&mut State, &mut TickContext<'a, Event>) -> Result<(), SystemError>;

struct System<State, Event> {
    order: u32,
    id: SystemId,
    rng: DeterministicRng,
    run: Box<SystemFunction<State, Event>>,
}

/// Ordered, single-threaded simulation schedule for a user-defined state type.
///
/// A system callback is an `Fn` (not a mutable closure) and must keep all
/// authoritative changes in `State`, use the supplied RNG, and emit events via
/// its `TickContext`. The scheduler snapshots state/RNG/events before each tick
/// and restores them on a system error. External side effects are forbidden in
/// callbacks because they cannot be rolled back.
pub struct Schedule<State, Event> {
    next_tick: u64,
    seed: u64,
    stream: u64,
    events: EventQueue<Event>,
    systems: Vec<System<State, Event>>,
}

impl<State, Event> Schedule<State, Event> {
    /// Creates a schedule at tick zero with deterministic RNG seed and stream.
    #[must_use]
    pub fn new(seed: u64, stream: u64) -> Self {
        Self {
            next_tick: 0,
            seed,
            stream,
            events: EventQueue::default(),
            systems: Vec::new(),
        }
    }

    /// Adds a system and orders all systems by `(order, SystemId)`.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleError::DuplicateSystemId`] if this ID is already
    /// registered, or [`ScheduleError::ScheduleAlreadyStarted`] if a tick has
    /// already been run.
    pub fn add_system<F>(
        &mut self,
        order: u32,
        id: SystemId,
        system: F,
    ) -> Result<(), ScheduleError>
    where
        F: for<'a> Fn(&mut State, &mut TickContext<'a, Event>) -> Result<(), SystemError> + 'static,
    {
        if self.next_tick != 0 {
            return Err(ScheduleError::ScheduleAlreadyStarted);
        }
        if self.systems.iter().any(|registered| registered.id == id) {
            return Err(ScheduleError::DuplicateSystemId(id));
        }
        self.systems.push(System {
            order,
            id,
            rng: DeterministicRng::new(self.seed, self.stream.wrapping_add(u64::from(id.0))),
            run: Box::new(system),
        });
        self.systems
            .sort_by_key(|registered| (registered.order, registered.id));
        Ok(())
    }

    /// Seed used to initialize the schedule's per-system random streams.
    #[must_use]
    pub const fn seed(&self) -> u64 {
        self.seed
    }

    /// Base stream selector used with each registered system ID.
    #[must_use]
    pub const fn stream(&self) -> u64 {
        self.stream
    }

    /// Tick expected by the next call to [`Self::run_tick`].
    #[must_use]
    pub const fn next_tick(&self) -> u64 {
        self.next_tick
    }

    /// Number of queued future events.
    #[must_use]
    pub fn pending_event_count(&self) -> usize {
        self.events.pending.len()
    }
}

impl<State: Clone, Event: Clone> Schedule<State, Event> {
    /// Runs exactly one tick with the corresponding immutable input snapshot.
    ///
    /// All systems see events delivered for this tick; event emission always
    /// targets the next tick. Systems execute serially in `(order, SystemId)`
    /// order. If a system fails, state, random stream, event queue, and tick
    /// position are restored to their pre-call values.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleError::InputTickMismatch`] when input is not for the
    /// next tick, [`ScheduleError::TickOverflow`] before advancing `u64::MAX`, or
    /// [`ScheduleError::SystemFailed`] when a system returns an error. No state
    /// is committed on error.
    pub fn run_tick(&mut self, state: &mut State, input: &InputFrame) -> Result<(), ScheduleError> {
        if input.tick != self.next_tick {
            return Err(ScheduleError::InputTickMismatch {
                expected: self.next_tick,
                actual: input.tick,
            });
        }
        let next_tick = self
            .next_tick
            .checked_add(1)
            .ok_or(ScheduleError::TickOverflow)?;

        let state_before = state.clone();
        let rng_before = self
            .systems
            .iter()
            .map(|system| system.rng.clone())
            .collect::<Vec<_>>();
        let events_before = self.events.clone();
        let current_events = self.events.take_due(self.next_tick);
        let tick = self.next_tick;

        for system in &mut self.systems {
            let System { id, rng, run, .. } = system;
            let system_id = *id;
            let mut context = TickContext {
                tick,
                producer: *id,
                input,
                events: &current_events,
                rng,
                outgoing: &mut self.events,
            };
            if let Err(error) = run(state, &mut context) {
                *state = state_before;
                for (system, rng) in self.systems.iter_mut().zip(rng_before) {
                    system.rng = rng;
                }
                self.events = events_before;
                return Err(ScheduleError::SystemFailed {
                    system: system_id,
                    error,
                });
            }
        }

        self.next_tick = next_tick;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RngError {
    EmptyRange,
}

impl std::fmt::Display for RngError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyRange => f.write_str("random range must have a positive upper bound"),
        }
    }
}

impl std::error::Error for RngError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemError {
    Failed { code: u32, message: String },
    EventSequenceOverflow,
}

impl SystemError {
    /// Creates a system failure with a stable machine-readable code.
    #[must_use]
    pub fn new(code: u32, message: impl Into<String>) -> Self {
        Self::Failed {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for SystemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed { code, message } => write!(f, "system error {code}: {message}"),
            Self::EventSequenceOverflow => f.write_str("system event sequence overflowed"),
        }
    }
}

impl std::error::Error for SystemError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleError {
    DuplicateSystemId(SystemId),
    ScheduleAlreadyStarted,
    InputTickMismatch {
        expected: u64,
        actual: u64,
    },
    TickOverflow,
    SystemFailed {
        system: SystemId,
        error: SystemError,
    },
}

impl std::fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateSystemId(id) => write!(f, "system ID {} is already registered", id.0),
            Self::ScheduleAlreadyStarted => {
                f.write_str("systems cannot be changed after the schedule starts")
            }
            Self::InputTickMismatch { expected, actual } => {
                write!(f, "expected input for tick {expected}, got tick {actual}")
            }
            Self::TickOverflow => f.write_str("simulation schedule tick counter overflowed"),
            Self::SystemFailed { system, error } => {
                write!(f, "system {} failed: {error}", system.0)
            }
        }
    }
}

impl std::error::Error for ScheduleError {}

#[cfg(test)]
mod tests {
    use super::{DeterministicRng, InputFrame, Schedule, ScheduleError, SystemError, SystemId};

    #[test]
    fn rng_matches_pcg_xsh_rr_64_32_reference_vector() {
        let mut rng = DeterministicRng::new(42, 54);
        assert_eq!(rng.next_u32(), 0xa15c_02b7);
        assert_eq!(rng.next_u32(), 0x7b47_f409);
        assert_eq!(rng.next_u32(), 0xba1d_3330);
    }

    #[test]
    fn rng_repeats_for_same_seed_and_stream_and_is_stream_specific() {
        let mut first = DeterministicRng::new(42, 7);
        let mut repeat = DeterministicRng::new(42, 7);
        let mut other_stream = DeterministicRng::new(42, 8);
        let values = (0..8).map(|_| first.next_u32()).collect::<Vec<_>>();
        assert_eq!(
            values,
            (0..8).map(|_| repeat.next_u32()).collect::<Vec<_>>()
        );
        assert_ne!(
            values,
            (0..8).map(|_| other_stream.next_u32()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn rng_bounded_draws_are_in_range_and_reject_empty_ranges() {
        let mut rng = DeterministicRng::new(1, 2);
        for _ in 0..1_000 {
            assert!(rng.below(7).unwrap() < 7);
        }
        assert!(rng.below(0).is_err());
    }

    #[test]
    fn unrelated_system_random_draws_do_not_perturb_each_other() {
        fn outcome(with_extra_system: bool) -> Vec<u32> {
            let mut schedule = Schedule::<Vec<u32>, ()>::new(123, 4);
            if with_extra_system {
                schedule
                    .add_system(0, SystemId::new(3), |_, context| {
                        for _ in 0..100 {
                            let _ = context.random_u32();
                        }
                        Ok(())
                    })
                    .unwrap();
            }
            schedule
                .add_system(1, SystemId::new(9), |state, context| {
                    state.push(context.random_u32());
                    Ok(())
                })
                .unwrap();
            let mut state = Vec::new();
            schedule.run_tick(&mut state, &InputFrame::new(0)).unwrap();
            state
        }

        assert_eq!(outcome(false), outcome(true));
    }

    #[test]
    fn serialized_input_frame_rejects_duplicate_action_ids() {
        let duplicate = r#"{"tick":0,"buttons":{"1":true,"1":false},"axes":{}}"#;
        assert!(serde_json::from_str::<InputFrame>(duplicate).is_err());
    }

    #[test]
    fn input_frame_defaults_missing_actions_and_stores_sorted_values() {
        let mut input = InputFrame::new(5);
        assert!(!input.button(10));
        assert_eq!(input.axis(10), 0);
        input.set_button(10, true);
        input.set_axis(3, i16::MIN);
        assert!(input.button(10));
        assert_eq!(input.axis(3), i16::MIN);
        assert_eq!(input.tick(), 5);
    }

    #[test]
    fn systems_run_in_order_then_system_id_order() {
        let mut schedule = Schedule::<Vec<u32>, ()>::new(3, 0);
        schedule
            .add_system(10, SystemId::new(9), |state, _| {
                state.push(9);
                Ok(())
            })
            .unwrap();
        schedule
            .add_system(0, SystemId::new(5), |state, _| {
                state.push(5);
                Ok(())
            })
            .unwrap();
        schedule
            .add_system(10, SystemId::new(4), |state, _| {
                state.push(4);
                Ok(())
            })
            .unwrap();
        schedule
            .run_tick(&mut Vec::new(), &InputFrame::new(0))
            .unwrap();
        let mut state = Vec::new();
        schedule.run_tick(&mut state, &InputFrame::new(1)).unwrap();
        assert_eq!(state, [5, 4, 9]);
    }

    #[test]
    fn schedule_systems_cannot_change_after_first_tick() {
        let mut schedule = Schedule::<u32, ()>::new(0, 0);
        schedule.run_tick(&mut 0, &InputFrame::new(0)).unwrap();
        assert_eq!(
            schedule.add_system(0, SystemId::new(1), |_, _| Ok(())),
            Err(ScheduleError::ScheduleAlreadyStarted)
        );
    }

    #[test]
    fn duplicate_system_id_and_wrong_input_tick_are_rejected() {
        let mut schedule = Schedule::<u32, ()>::new(0, 0);
        schedule
            .add_system(0, SystemId::new(1), |_, _| Ok(()))
            .unwrap();
        assert_eq!(
            schedule.add_system(1, SystemId::new(1), |_, _| Ok(())),
            Err(ScheduleError::DuplicateSystemId(SystemId::new(1)))
        );
        let mut state = 0;
        assert_eq!(
            schedule.run_tick(&mut state, &InputFrame::new(1)),
            Err(ScheduleError::InputTickMismatch {
                expected: 0,
                actual: 1
            })
        );
        assert_eq!(state, 0);
        assert_eq!(schedule.next_tick(), 0);
    }

    #[test]
    fn emitted_events_are_delivered_next_tick_in_canonical_order() {
        let mut schedule = Schedule::<Vec<u32>, u32>::new(0, 0);
        schedule
            .add_system(10, SystemId::new(9), |_, context| {
                if context.tick() == 0 {
                    context.emit(9).unwrap();
                    context.emit(10).unwrap();
                }
                Ok(())
            })
            .unwrap();
        schedule
            .add_system(0, SystemId::new(2), |_, context| {
                if context.tick() == 0 {
                    context.emit(2).unwrap();
                }
                Ok(())
            })
            .unwrap();
        schedule
            .add_system(20, SystemId::new(20), |state, context| {
                if context.tick() == 1 {
                    state.extend(context.events().iter().map(|event| *event.payload()));
                }
                Ok(())
            })
            .unwrap();
        let mut state = Vec::new();
        schedule.run_tick(&mut state, &InputFrame::new(0)).unwrap();
        assert_eq!(schedule.pending_event_count(), 3);
        schedule.run_tick(&mut state, &InputFrame::new(1)).unwrap();
        assert_eq!(state, [2, 9, 10]);
        assert_eq!(schedule.pending_event_count(), 0);
    }

    #[test]
    fn failed_tick_restores_state_rng_and_events() {
        let mut schedule = Schedule::<u32, u32>::new(42, 9);
        schedule
            .add_system(0, SystemId::new(1), |state, context| {
                *state += context.random_u32();
                context.emit(99)?;
                Ok(())
            })
            .unwrap();
        schedule
            .add_system(1, SystemId::new(2), |_, _| {
                Err(SystemError::new(7, "deterministic test failure"))
            })
            .unwrap();
        let rng_before = schedule.systems[0].rng.clone();
        let mut state = 5;
        assert!(matches!(
            schedule.run_tick(&mut state, &InputFrame::new(0)),
            Err(ScheduleError::SystemFailed { system, .. }) if system == SystemId::new(2)
        ));
        assert_eq!(state, 5);
        assert_eq!(schedule.next_tick(), 0);
        assert_eq!(schedule.pending_event_count(), 0);
        assert_eq!(schedule.systems[0].rng, rng_before);
    }
}
