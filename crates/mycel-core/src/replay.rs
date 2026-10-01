//! Bounded, versioned JSON replay capture and headless playback.

use std::io::Write;

use serde::{Deserialize, Serialize};

use crate::{InputFrame, Schedule, ScheduleError};

/// Current replay schema version.
pub const REPLAY_SCHEMA_VERSION: u16 = 1;
/// Maximum accepted UTF-8 JSON replay size.
pub const MAX_REPLAY_JSON_BYTES: usize = 16 * 1024 * 1024;
/// Maximum number of tick frames retained in one replay.
pub const MAX_REPLAY_FRAMES: usize = 250_000;
/// Maximum combined digital/analog actions in any input frame.
pub const MAX_ACTIONS_PER_FRAME: usize = 1_024;

const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const TARGET_TRIPLE: &str = "x86_64-apple-darwin";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TARGET_TRIPLE: &str = "aarch64-apple-darwin";
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const TARGET_TRIPLE: &str = "x86_64-unknown-linux-gnu";
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const TARGET_TRIPLE: &str = "aarch64-unknown-linux-gnu";
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
const TARGET_TRIPLE: &str = "x86_64-pc-windows-msvc";
#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
const TARGET_TRIPLE: &str = "aarch64-pc-windows-msvc";
#[cfg(not(any(
    all(target_os = "macos", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "aarch64"),
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "windows", target_arch = "aarch64")
)))]
const TARGET_TRIPLE: &str = "unknown-target";
const MAX_ENGINE_VERSION_BYTES: usize = 128;

/// Versioned replay metadata needed to reproduce a schedule input sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayHeader {
    schema_version: u16,
    engine_version: String,
    target_triple: String,
    ticks_per_second: u32,
    seed: u64,
    stream: u64,
}

impl ReplayHeader {
    /// Replay schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }

    /// Exact engine version that recorded the replay.
    #[must_use]
    pub fn engine_version(&self) -> &str {
        &self.engine_version
    }

    /// Native compile target that recorded the replay.
    #[must_use]
    pub fn target_triple(&self) -> &str {
        &self.target_triple
    }

    /// Simulation tick rate required for playback.
    #[must_use]
    pub const fn ticks_per_second(&self) -> u32 {
        self.ticks_per_second
    }

    /// Seed used to initialize the schedule's per-system RNG streams.
    #[must_use]
    pub const fn seed(&self) -> u64 {
        self.seed
    }

    /// Base stream selector used with each system ID.
    #[must_use]
    pub const fn stream(&self) -> u64 {
        self.stream
    }
}

/// Input recording for deterministic headless playback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Replay {
    header: ReplayHeader,
    frames: Vec<InputFrame>,
}

impl Replay {
    /// Creates and validates a replay for this engine version.
    ///
    /// Frames must begin at tick zero and be contiguous. The returned replay is
    /// portable JSON data; it contains no game state, assets, or executable code.
    ///
    /// # Errors
    ///
    /// Returns [`ReplayError`] for an invalid tick rate, oversized recording,
    /// invalid engine-version text, action-limit violations, or noncontiguous
    /// input ticks.
    pub fn new(
        ticks_per_second: u32,
        seed: u64,
        stream: u64,
        frames: Vec<InputFrame>,
    ) -> Result<Self, ReplayError> {
        let replay = Self {
            header: ReplayHeader {
                schema_version: REPLAY_SCHEMA_VERSION,
                engine_version: ENGINE_VERSION.to_owned(),
                target_triple: TARGET_TRIPLE.to_owned(),
                ticks_per_second,
                seed,
                stream,
            },
            frames,
        };
        replay.validate()?;
        Ok(replay)
    }

    /// Parses bounded UTF-8 JSON and validates its schema and contents.
    ///
    /// Unknown fields are rejected. The input byte limit is checked before JSON
    /// parsing to bound allocation and parsing work.
    ///
    /// # Errors
    ///
    /// Returns [`ReplayError`] for malformed JSON, unsupported schema versions,
    /// invalid metadata, out-of-order ticks, or resource-limit violations.
    pub fn from_json(json: &[u8]) -> Result<Self, ReplayError> {
        if json.len() > MAX_REPLAY_JSON_BYTES {
            return Err(ReplayError::JsonTooLarge {
                actual: json.len(),
                maximum: MAX_REPLAY_JSON_BYTES,
            });
        }
        let replay: Self = serde_json::from_slice(json)
            .map_err(|error| ReplayError::InvalidJson(error.to_string()))?;
        replay.validate()?;
        Ok(replay)
    }

    /// Serializes the replay to compact, deterministic JSON.
    ///
    /// # Errors
    ///
    /// Returns [`ReplayError::JsonSerialization`] if serialization fails, or a
    /// validation/resource-limit error if the replay is too large to persist.
    pub fn to_json(&self) -> Result<Vec<u8>, ReplayError> {
        self.validate()?;
        let mut output = LimitedBuffer::default();
        if let Err(error) = serde_json::to_writer(&mut output, self) {
            if let Some(actual) = output.overflow_at {
                return Err(ReplayError::JsonTooLarge {
                    actual,
                    maximum: MAX_REPLAY_JSON_BYTES,
                });
            }
            return Err(ReplayError::JsonSerialization(error.to_string()));
        }
        Ok(output.bytes)
    }

    /// Replay metadata.
    #[must_use]
    pub const fn header(&self) -> &ReplayHeader {
        &self.header
    }

    /// Recorded tick-indexed input snapshots.
    #[must_use]
    pub fn frames(&self) -> &[InputFrame] {
        &self.frames
    }

    /// Replays all inputs into a fresh schedule and state.
    ///
    /// Playback requires exact engine version, target triple, tick rate, seed,
    /// and base stream equality. Systems must already be registered in the same deterministic
    /// order. If a later tick fails, earlier successful ticks remain committed;
    /// the failing tick itself is failure-atomic via [`Schedule::run_tick`].
    ///
    /// # Errors
    ///
    /// Returns [`ReplayError`] for incompatible metadata, a non-fresh schedule,
    /// or the tick-specific schedule failure.
    pub fn playback<State: Clone, Event: Clone>(
        &self,
        schedule: &mut Schedule<State, Event>,
        state: &mut State,
        ticks_per_second: u32,
    ) -> Result<(), ReplayError> {
        self.validate()?;
        if self.header.engine_version != ENGINE_VERSION {
            return Err(ReplayError::EngineVersionMismatch {
                recorded: self.header.engine_version.clone(),
                current: ENGINE_VERSION.to_owned(),
            });
        }
        if self.header.target_triple != TARGET_TRIPLE {
            return Err(ReplayError::TargetMismatch {
                recorded: self.header.target_triple.clone(),
                current: TARGET_TRIPLE.to_owned(),
            });
        }
        if self.header.ticks_per_second != ticks_per_second {
            return Err(ReplayError::TickRateMismatch {
                recorded: self.header.ticks_per_second,
                actual: ticks_per_second,
            });
        }
        if self.header.seed != schedule.seed() {
            return Err(ReplayError::SeedMismatch {
                recorded: self.header.seed,
                actual: schedule.seed(),
            });
        }
        if self.header.stream != schedule.stream() {
            return Err(ReplayError::StreamMismatch {
                recorded: self.header.stream,
                actual: schedule.stream(),
            });
        }
        if schedule.next_tick() != 0 {
            return Err(ReplayError::ScheduleNotFresh(schedule.next_tick()));
        }

        for frame in &self.frames {
            schedule
                .run_tick(state, frame)
                .map_err(|error| ReplayError::TickFailed {
                    tick: frame.tick(),
                    error,
                })?;
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), ReplayError> {
        if self.header.schema_version != REPLAY_SCHEMA_VERSION {
            return Err(ReplayError::UnsupportedSchema(self.header.schema_version));
        }
        if self.header.ticks_per_second == 0 {
            return Err(ReplayError::ZeroTickRate);
        }
        if self.header.engine_version.is_empty()
            || self.header.engine_version.len() > MAX_ENGINE_VERSION_BYTES
            || self.header.engine_version.chars().any(char::is_control)
            || self.header.target_triple.is_empty()
            || self.header.target_triple.len() > MAX_ENGINE_VERSION_BYTES
            || self.header.target_triple.chars().any(char::is_control)
        {
            return Err(ReplayError::InvalidEngineVersion);
        }
        if self.frames.len() > MAX_REPLAY_FRAMES {
            return Err(ReplayError::TooManyFrames {
                actual: self.frames.len(),
                maximum: MAX_REPLAY_FRAMES,
            });
        }

        let mut expected_tick = 0_u64;
        for frame in &self.frames {
            if frame.action_count() > MAX_ACTIONS_PER_FRAME {
                return Err(ReplayError::TooManyActions {
                    tick: frame.tick(),
                    actual: frame.action_count(),
                    maximum: MAX_ACTIONS_PER_FRAME,
                });
            }
            if frame.tick() != expected_tick {
                return Err(ReplayError::NonContiguousTicks {
                    expected: expected_tick,
                    actual: frame.tick(),
                });
            }
            expected_tick = expected_tick
                .checked_add(1)
                .ok_or(ReplayError::TooManyFrames {
                    actual: self.frames.len(),
                    maximum: MAX_REPLAY_FRAMES,
                })?;
        }
        Ok(())
    }
}

#[derive(Default)]
struct LimitedBuffer {
    bytes: Vec<u8>,
    overflow_at: Option<usize>,
}

impl Write for LimitedBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let attempted = self.bytes.len().saturating_add(bytes.len());
        if attempted > MAX_REPLAY_JSON_BYTES {
            self.overflow_at = Some(attempted);
            return Err(std::io::Error::other("replay JSON size limit exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayError {
    JsonTooLarge {
        actual: usize,
        maximum: usize,
    },
    InvalidJson(String),
    JsonSerialization(String),
    UnsupportedSchema(u16),
    ZeroTickRate,
    InvalidEngineVersion,
    TooManyFrames {
        actual: usize,
        maximum: usize,
    },
    TooManyActions {
        tick: u64,
        actual: usize,
        maximum: usize,
    },
    NonContiguousTicks {
        expected: u64,
        actual: u64,
    },
    EngineVersionMismatch {
        recorded: String,
        current: String,
    },
    TargetMismatch {
        recorded: String,
        current: String,
    },
    TickRateMismatch {
        recorded: u32,
        actual: u32,
    },
    SeedMismatch {
        recorded: u64,
        actual: u64,
    },
    StreamMismatch {
        recorded: u64,
        actual: u64,
    },
    ScheduleNotFresh(u64),
    TickFailed {
        tick: u64,
        error: ScheduleError,
    },
}

impl std::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::JsonTooLarge { actual, maximum } => {
                write!(f, "replay JSON is {actual} bytes; maximum is {maximum}")
            }
            Self::InvalidJson(message) => write!(f, "invalid replay JSON: {message}"),
            Self::JsonSerialization(message) => write!(f, "could not serialize replay: {message}"),
            Self::UnsupportedSchema(version) => {
                write!(f, "unsupported replay schema version {version}")
            }
            Self::ZeroTickRate => f.write_str("replay tick rate must be greater than zero"),
            Self::InvalidEngineVersion => f.write_str("replay engine version is invalid"),
            Self::TooManyFrames { actual, maximum } => {
                write!(f, "replay has {actual} frames; maximum is {maximum}")
            }
            Self::TooManyActions {
                tick,
                actual,
                maximum,
            } => write!(
                f,
                "replay tick {tick} has {actual} actions; maximum is {maximum}"
            ),
            Self::NonContiguousTicks { expected, actual } => {
                write!(
                    f,
                    "expected replay input for tick {expected}, got tick {actual}"
                )
            }
            Self::EngineVersionMismatch { recorded, current } => write!(
                f,
                "replay engine version {recorded} does not match current version {current}"
            ),
            Self::TargetMismatch { recorded, current } => {
                write!(
                    f,
                    "replay target {recorded} does not match current target {current}"
                )
            }
            Self::TickRateMismatch { recorded, actual } => write!(
                f,
                "replay tick rate {recorded} does not match current rate {actual}"
            ),
            Self::SeedMismatch { recorded, actual } => {
                write!(
                    f,
                    "replay seed {recorded} does not match schedule seed {actual}"
                )
            }
            Self::StreamMismatch { recorded, actual } => write!(
                f,
                "replay stream {recorded} does not match schedule stream {actual}"
            ),
            Self::ScheduleNotFresh(tick) => {
                write!(
                    f,
                    "replay requires a fresh schedule, which is at tick {tick}"
                )
            }
            Self::TickFailed { tick, error } => write!(f, "replay failed at tick {tick}: {error}"),
        }
    }
}

impl std::error::Error for ReplayError {}

#[cfg(test)]
mod tests {
    use super::{REPLAY_SCHEMA_VERSION, Replay, ReplayError};
    use crate::{InputFrame, Schedule, SystemId};

    fn frames(count: u64) -> Vec<InputFrame> {
        (0..count)
            .map(|tick| {
                let mut frame = InputFrame::new(tick);
                frame.set_button(1, tick % 2 == 0);
                frame
            })
            .collect()
    }

    #[test]
    fn replay_json_round_trips_with_stable_order_and_versioned_metadata() {
        let mut first = InputFrame::new(0);
        first.set_button(7, true);
        first.set_axis(2, -32_768);
        let replay = Replay::new(60, 42, 9, vec![first, InputFrame::new(1)]).unwrap();
        let json = replay.to_json().unwrap();
        let decoded = Replay::from_json(&json).unwrap();
        assert_eq!(decoded, replay);
        assert_eq!(decoded.header().schema_version(), REPLAY_SCHEMA_VERSION);
        assert_eq!(decoded.header().ticks_per_second(), 60);
        assert_eq!(json, decoded.to_json().unwrap());
    }

    #[test]
    fn rejects_unknown_fields_invalid_schema_and_noncontiguous_ticks() {
        let unknown_field = br#"{"header":{"schema_version":1,"engine_version":"0.1.0","target_triple":"x86_64-apple-darwin","ticks_per_second":60,"seed":1,"stream":0,"extra":true},"frames":[]}"#;
        assert!(matches!(
            Replay::from_json(unknown_field),
            Err(ReplayError::InvalidJson(_))
        ));

        let wrong_version = br#"{"header":{"schema_version":99,"engine_version":"0.1.0","target_triple":"x86_64-apple-darwin","ticks_per_second":60,"seed":1,"stream":0},"frames":[]}"#;
        assert_eq!(
            Replay::from_json(wrong_version),
            Err(ReplayError::UnsupportedSchema(99))
        );

        let skipped_tick = Replay::new(60, 0, 0, vec![InputFrame::new(0), InputFrame::new(2)]);
        assert_eq!(
            skipped_tick,
            Err(ReplayError::NonContiguousTicks {
                expected: 1,
                actual: 2
            })
        );
    }

    #[test]
    fn replay_playback_matches_a_fresh_schedule_and_checks_compatibility() {
        let replay = Replay::new(60, 42, 7, frames(4)).unwrap();
        let build_schedule = || {
            let mut schedule = Schedule::<Vec<u32>, ()>::new(42, 7);
            schedule
                .add_system(0, SystemId::new(1), |state, context| {
                    state.push(context.random_u32() ^ u32::from(context.input().button(1)));
                    Ok(())
                })
                .unwrap();
            schedule
        };
        let mut schedule = build_schedule();
        let mut state = Vec::new();
        replay.playback(&mut schedule, &mut state, 60).unwrap();
        assert_eq!(state.len(), 4);
        assert_eq!(schedule.next_tick(), 4);

        let mut wrong_seed = Schedule::<Vec<u32>, ()>::new(43, 7);
        let mut other_state = Vec::new();
        assert!(matches!(
            replay.playback(&mut wrong_seed, &mut other_state, 60),
            Err(ReplayError::SeedMismatch { .. })
        ));
        assert!(matches!(
            replay.playback(&mut build_schedule(), &mut other_state, 30),
            Err(ReplayError::TickRateMismatch { .. })
        ));
    }

    #[test]
    fn replay_enforces_per_frame_action_limit() {
        let mut frame = InputFrame::new(0);
        for action_id in 0_u16..=1_024 {
            frame.set_button(action_id, true);
        }
        assert!(matches!(
            Replay::new(60, 0, 0, vec![frame]),
            Err(ReplayError::TooManyActions {
                actual: 1_025,
                maximum: 1_024,
                ..
            })
        ));
    }

    #[test]
    fn replay_rejects_wrong_target_before_running_any_tick() {
        let mut replay = Replay::new(60, 42, 7, frames(1)).unwrap();
        replay.header.target_triple = "different-target".to_owned();
        let mut schedule = Schedule::<Vec<u32>, ()>::new(42, 7);
        schedule
            .add_system(0, SystemId::new(1), |state, _| {
                state.push(1);
                Ok(())
            })
            .unwrap();
        let mut state = Vec::new();
        assert!(matches!(
            replay.playback(&mut schedule, &mut state, 60),
            Err(ReplayError::TargetMismatch { .. })
        ));
        assert_eq!(state, Vec::<u32>::new());
        assert_eq!(schedule.next_tick(), 0);
    }

    #[test]
    fn replay_rejects_json_over_limit_before_parsing() {
        let too_large = vec![b' '; super::MAX_REPLAY_JSON_BYTES + 1];
        assert!(matches!(
            Replay::from_json(&too_large),
            Err(ReplayError::JsonTooLarge { .. })
        ));
    }
}
