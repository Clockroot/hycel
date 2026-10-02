//! Bounded Kira-backed audio output kept outside authoritative simulation APIs.
//!
//! Sound clips own encoded audio bytes and validate their container before playback. The wrapper
//! exposes no Kira or device types; unavailable audio devices and voice saturation degrade to
//! explicit skipped-playback outcomes and never require gameplay to fail.

use std::{collections::VecDeque, error::Error, fmt, io::Cursor, sync::Arc};

use kira::{
    AudioManager, AudioManagerSettings, DefaultBackend, Tween,
    sound::{
        FromFileError, PlaybackState,
        streaming::{StreamingSoundData, StreamingSoundHandle},
    },
};

/// Maximum encoded bytes held for one authored audio clip.
pub const MAX_AUDIO_CLIP_BYTES: usize = 64 * 1024 * 1024;
/// Maximum concurrent short sound-effect voices. Background music has one separate voice.
pub const MAX_EFFECT_VOICES: usize = 32;
/// Maximum pending diagnostics retained between host polls.
pub const MAX_PENDING_AUDIO_DIAGNOSTICS: usize = 64;

/// A bounded, validated audio source that can be played more than once.
#[derive(Clone, Debug)]
pub struct AudioClip {
    encoded: Arc<[u8]>,
}

impl AudioClip {
    /// Validates a supported audio source and stores its bounded encoded bytes.
    ///
    /// Kira uses Symphonia to probe the container here; streamed decoding keeps long tracks out
    /// of the game's main and audio callback threads. Supported demuxers/codecs are limited by
    /// the enabled crate features (`WAV`, `FLAC`, `MP3`, and `Ogg/Vorbis`).
    ///
    /// # Errors
    ///
    /// Returns [`AudioError`] for an empty, oversized, unsupported, or malformed clip.
    pub fn parse(encoded: &[u8]) -> Result<Self, AudioError> {
        if encoded.is_empty() {
            return Err(AudioError::EmptyClip);
        }
        if encoded.len() > MAX_AUDIO_CLIP_BYTES {
            return Err(AudioError::ClipTooLarge {
                actual_bytes: encoded.len(),
                maximum_bytes: MAX_AUDIO_CLIP_BYTES,
            });
        }
        let owned: Arc<[u8]> = Arc::from(encoded);
        StreamingSoundData::from_cursor(Cursor::new(Arc::clone(&owned)))
            .map_err(|error| AudioError::InvalidClip(error.to_string()))?;
        Ok(Self { encoded: owned })
    }

    /// Encoded source byte length.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        self.encoded.len()
    }
}

/// Audio device lifecycle as observed by Hycel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioStatus {
    /// Kira opened an output device successfully.
    Active,
    /// Device startup failed; playback is silently skipped and the reason is inspectable.
    Unavailable { reason: String },
}

/// Result of a best-effort presentation playback request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackOutcome {
    /// The backend accepted the sound.
    Started,
    /// Audio output is unavailable, so no sound was played.
    SkippedUnavailable,
    /// The bounded concurrent sound-effect voice limit was reached.
    SkippedVoiceLimit,
    /// The decoder/backend rejected playback; inspect [`AudioService::poll_errors`].
    SkippedError,
}

type KiraSoundHandle = StreamingSoundHandle<FromFileError>;

struct ActiveBackend {
    manager: AudioManager<DefaultBackend>,
    effects: Vec<KiraSoundHandle>,
    music: Option<KiraSoundHandle>,
}

enum OutputBackend {
    Active(Box<ActiveBackend>),
    Unavailable(String),
}

/// Best-effort audio service with one looping music voice and bounded effect voices.
pub struct AudioService {
    backend: OutputBackend,
    pending_errors: VecDeque<AudioError>,
}

impl AudioService {
    /// Opens the platform's default output device, falling back to silent operation on failure.
    #[must_use]
    pub fn open() -> Self {
        match AudioManager::<DefaultBackend>::new(AudioManagerSettings::default()) {
            Ok(manager) => Self {
                backend: OutputBackend::Active(Box::new(ActiveBackend {
                    manager,
                    effects: Vec::new(),
                    music: None,
                })),
                pending_errors: VecDeque::new(),
            },
            Err(error) => Self::silent(error.to_string()),
        }
    }

    /// Creates a silent service, useful for headless hosts and explicit output fallback.
    #[must_use]
    pub fn silent(reason: impl Into<String>) -> Self {
        Self {
            backend: OutputBackend::Unavailable(reason.into()),
            pending_errors: VecDeque::new(),
        }
    }

    /// Current device state and, when silent, its startup failure reason.
    #[must_use]
    pub fn status(&self) -> AudioStatus {
        match &self.backend {
            OutputBackend::Active(_) => AudioStatus::Active,
            OutputBackend::Unavailable(reason) => AudioStatus::Unavailable {
                reason: reason.clone(),
            },
        }
    }

    /// Drains decoder/backend failures from recent requests and active voices.
    ///
    /// Errors are presentation-only and must not be fed into authoritative game state.
    pub fn poll_errors(&mut self) -> Vec<AudioError> {
        let mut errors = std::mem::take(&mut self.pending_errors)
            .into_iter()
            .collect::<Vec<_>>();
        let OutputBackend::Active(active) = &mut self.backend else {
            return errors;
        };
        for handle in active.effects.iter_mut().chain(active.music.iter_mut()) {
            while let Some(error) = handle.pop_error() {
                errors.push(AudioError::InvalidClip(error.to_string()));
            }
        }
        errors
    }

    /// Plays a short effect without changing simulation state or timeline.
    ///
    /// Call this at a host/presentation boundary in response to derived events, not from an
    /// authoritative simulation callback that may be retried or rolled back.
    /// Decoder/backend failures are queued for [`Self::poll_errors`] and returned as a skipped
    /// outcome, so presentation failures cannot abort gameplay.
    pub fn play_effect(&mut self, clip: &AudioClip) -> PlaybackOutcome {
        let OutputBackend::Active(active) = &mut self.backend else {
            return PlaybackOutcome::SkippedUnavailable;
        };
        let ActiveBackend {
            manager, effects, ..
        } = active.as_mut();
        effects.retain(|handle| handle.state() != PlaybackState::Stopped);
        if effects.len() >= MAX_EFFECT_VOICES {
            return PlaybackOutcome::SkippedVoiceLimit;
        }
        let sound = match streaming_data(clip, false) {
            Ok(sound) => sound,
            Err(error) => {
                record_error(&mut self.pending_errors, error);
                return PlaybackOutcome::SkippedError;
            }
        };
        match manager.play(sound) {
            Ok(handle) => {
                effects.push(handle);
                PlaybackOutcome::Started
            }
            Err(error) => {
                record_error(
                    &mut self.pending_errors,
                    AudioError::Backend(error.to_string()),
                );
                PlaybackOutcome::SkippedError
            }
        }
    }

    /// Starts or replaces the single looping background-music voice.
    ///
    /// Call this from host/presentation orchestration; wall-clock audio playback is not part of
    /// simulation time, replay state, or rollback.
    /// Decoder/backend failures are queued for [`Self::poll_errors`] and returned as a skipped
    /// outcome, so presentation failures cannot abort gameplay.
    pub fn play_music(&mut self, clip: &AudioClip) -> PlaybackOutcome {
        let OutputBackend::Active(active) = &mut self.backend else {
            return PlaybackOutcome::SkippedUnavailable;
        };
        let ActiveBackend { manager, music, .. } = active.as_mut();
        if let Some(previous) = music.as_mut() {
            previous.stop(Tween::default());
        }
        let sound = match streaming_data(clip, true) {
            Ok(sound) => sound,
            Err(error) => {
                record_error(&mut self.pending_errors, error);
                return PlaybackOutcome::SkippedError;
            }
        };
        match manager.play(sound) {
            Ok(handle) => {
                *music = Some(handle);
                PlaybackOutcome::Started
            }
            Err(error) => {
                record_error(
                    &mut self.pending_errors,
                    AudioError::Backend(error.to_string()),
                );
                PlaybackOutcome::SkippedError
            }
        }
    }
}

fn record_error(pending_errors: &mut VecDeque<AudioError>, error: AudioError) {
    if pending_errors.len() == MAX_PENDING_AUDIO_DIAGNOSTICS {
        pending_errors.pop_front();
    }
    pending_errors.push_back(error);
}

fn streaming_data(
    clip: &AudioClip,
    looping: bool,
) -> Result<StreamingSoundData<FromFileError>, AudioError> {
    let sound = StreamingSoundData::from_cursor(Cursor::new(Arc::clone(&clip.encoded)))
        .map_err(|error| AudioError::InvalidClip(error.to_string()))?;
    Ok(if looping {
        sound.loop_region(..)
    } else {
        sound
    })
}

/// Recoverable audio input/device/playback error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioError {
    /// Empty audio input cannot be played.
    EmptyClip,
    /// Encoded input exceeded the per-clip memory bound.
    ClipTooLarge {
        /// Actual input size.
        actual_bytes: usize,
        /// Maximum accepted input size.
        maximum_bytes: usize,
    },
    /// Kira/Symphonia rejected or could not decode the audio source.
    InvalidClip(String),
    /// Kira's output backend failed after successful startup.
    Backend(String),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyClip => f.write_str("audio clip is empty"),
            Self::ClipTooLarge {
                actual_bytes,
                maximum_bytes,
            } => write!(
                f,
                "audio clip is {actual_bytes} bytes; limit is {maximum_bytes} bytes"
            ),
            Self::InvalidClip(message) => write!(f, "invalid or unsupported audio clip: {message}"),
            Self::Backend(message) => write!(f, "audio playback failed: {message}"),
        }
    }
}

impl Error for AudioError {}

#[cfg(test)]
mod tests {
    use super::{
        AudioClip, AudioError, AudioService, AudioStatus, MAX_AUDIO_CLIP_BYTES, PlaybackOutcome,
    };

    fn minimal_pcm_wav() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&38_u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&8_000_u32.to_le_bytes());
        bytes.extend_from_slice(&16_000_u32.to_le_bytes());
        bytes.extend_from_slice(&2_u16.to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&2_u32.to_le_bytes());
        bytes.extend_from_slice(&0_i16.to_le_bytes());
        bytes
    }

    #[test]
    fn parses_bounded_wav_clip_and_allows_repeat_use() {
        let bytes = minimal_pcm_wav();
        let clip = AudioClip::parse(&bytes).unwrap();
        assert_eq!(clip.encoded_len(), bytes.len());
        let clone = clip.clone();
        assert_eq!(clone.encoded_len(), bytes.len());
    }

    #[test]
    fn rejects_empty_oversized_and_unsupported_audio_inputs() {
        assert_eq!(AudioClip::parse(&[]).unwrap_err(), AudioError::EmptyClip);
        let oversized = vec![0; MAX_AUDIO_CLIP_BYTES + 1];
        assert!(matches!(
            AudioClip::parse(&oversized),
            Err(AudioError::ClipTooLarge { .. })
        ));
        assert!(matches!(
            AudioClip::parse(b"not an audio file"),
            Err(AudioError::InvalidClip(_))
        ));
    }

    #[test]
    fn pending_audio_diagnostics_remain_bounded() {
        let mut pending = std::collections::VecDeque::new();
        for index in 0..100 {
            super::record_error(&mut pending, AudioError::Backend(format!("error-{index}")));
        }
        assert_eq!(pending.len(), super::MAX_PENDING_AUDIO_DIAGNOSTICS);
        assert_eq!(
            pending.front(),
            Some(&AudioError::Backend("error-36".to_owned()))
        );
        assert_eq!(
            pending.back(),
            Some(&AudioError::Backend("error-99".to_owned()))
        );
    }

    #[test]
    fn silent_output_is_inspectable_and_never_fails_gameplay_play_requests() {
        let clip = AudioClip::parse(&minimal_pcm_wav()).unwrap();
        let mut audio = AudioService::silent("no device in headless test");
        assert_eq!(
            audio.status(),
            AudioStatus::Unavailable {
                reason: "no device in headless test".to_owned()
            }
        );
        assert_eq!(
            audio.play_effect(&clip),
            PlaybackOutcome::SkippedUnavailable
        );
        assert_eq!(audio.play_music(&clip), PlaybackOutcome::SkippedUnavailable);
    }
}
