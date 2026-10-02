//! Tick-driven animation playback and explicit scene-transition requests.
//!
//! Clip definitions are authored and validated by `hycel-project`; this crate never stores
//! renderer-local texture handles. Frame IDs remain stable project resource UUIDs until the
//! presentation host resolves them to its own GPU resources.

use std::{error::Error, fmt};

use hycel_project::{AnimationClipDocument, SceneDocument};

/// One event emitted by deterministic animation playback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimationEvent {
    /// A non-looping clip reached its final frame and will remain there.
    Finished,
    /// A looping clip wrapped from its last frame to its first.
    Looped,
}

/// Cloneable animation state suitable for enclosing simulation rollback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnimationPlayer {
    clip: AnimationClipDocument,
    frame_index: usize,
    ticks_in_frame: u32,
    last_tick: Option<u64>,
    finished: bool,
}

impl AnimationPlayer {
    /// Starts an authored clip at its first frame.
    #[must_use]
    pub fn new(clip: AnimationClipDocument) -> Self {
        Self {
            clip,
            frame_index: 0,
            ticks_in_frame: 0,
            last_tick: None,
            finished: false,
        }
    }

    /// Current frame's stable project texture UUID.
    #[must_use]
    pub fn texture_id(&self) -> &str {
        self.clip.frames()[self.frame_index].texture_id()
    }

    /// Current zero-based clip frame index.
    #[must_use]
    pub const fn frame_index(&self) -> usize {
        self.frame_index
    }

    /// Whether a non-looping clip has completed.
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        self.finished
    }

    /// Advances playback for one explicit simulation tick.
    ///
    /// The first call establishes the starting tick and displays frame zero. Further calls must
    /// be contiguous. The returned completion/wrap event is deterministic from the clip and tick
    /// sequence, so it can be regenerated while replaying recorded input.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationError`] if a tick is repeated, skipped, or overflows.
    pub fn advance(&mut self, tick: u64) -> Result<Option<AnimationEvent>, AnimationError> {
        if let Some(previous_tick) = self.last_tick {
            if previous_tick.checked_add(1) != Some(tick) {
                return Err(AnimationError {
                    expected_tick: previous_tick.saturating_add(1),
                    actual_tick: tick,
                });
            }
        } else {
            self.last_tick = Some(tick);
            return Ok(None);
        }
        self.last_tick = Some(tick);
        if self.finished {
            return Ok(None);
        }
        self.ticks_in_frame += 1;
        let current_duration = self.clip.frames()[self.frame_index].duration_ticks();
        if self.ticks_in_frame < current_duration {
            return Ok(None);
        }
        self.ticks_in_frame = 0;
        if self.frame_index + 1 < self.clip.frames().len() {
            self.frame_index += 1;
            return Ok(None);
        }
        if self.clip.looping() {
            self.frame_index = 0;
            return Ok(Some(AnimationEvent::Looped));
        }
        self.finished = true;
        self.ticks_in_frame = current_duration;
        Ok(Some(AnimationEvent::Finished))
    }
}

/// Animation tick sequencing error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnimationError {
    expected_tick: u64,
    actual_tick: u64,
}

impl AnimationError {
    /// Tick required after the last successful advance.
    #[must_use]
    pub const fn expected_tick(self) -> u64 {
        self.expected_tick
    }

    /// Tick supplied by the caller.
    #[must_use]
    pub const fn actual_tick(self) -> u64 {
        self.actual_tick
    }
}

impl fmt::Display for AnimationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "expected animation tick {}, got {}",
            self.expected_tick, self.actual_tick
        )
    }
}

impl Error for AnimationError {}

/// A pending scene transition requested by simulation code and applied on a later tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SceneTransitionQueue {
    current_scene_id: String,
    pending: Option<SceneTransitionRequest>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SceneTransitionRequest {
    target_scene_id: String,
    requested_tick: u64,
}

impl SceneTransitionQueue {
    /// Creates a queue for the currently loaded scene UUID.
    #[must_use]
    pub fn new(current_scene_id: impl Into<String>) -> Self {
        Self {
            current_scene_id: current_scene_id.into(),
            pending: None,
        }
    }

    /// Current scene UUID.
    #[must_use]
    pub fn current_scene_id(&self) -> &str {
        &self.current_scene_id
    }

    /// Requests a transition to `target_scene_id` during `requested_tick`.
    ///
    /// A transition is committed no earlier than the following tick, at the host's explicit
    /// scene-update boundary. Only one request may be pending.
    ///
    /// # Errors
    ///
    /// Returns [`SceneTransitionError`] for an empty/current target or an existing pending
    /// request. Scene existence is checked when the host applies the transition.
    pub fn request(
        &mut self,
        target_scene_id: impl Into<String>,
        requested_tick: u64,
    ) -> Result<(), SceneTransitionError> {
        let target_scene_id = target_scene_id.into();
        if target_scene_id.is_empty() || target_scene_id == self.current_scene_id {
            return Err(SceneTransitionError::new(
                SceneTransitionErrorKind::InvalidTarget,
                target_scene_id,
                requested_tick,
                "target scene must be non-empty and different from the current scene",
            ));
        }
        if self.pending.is_some() {
            return Err(SceneTransitionError::new(
                SceneTransitionErrorKind::AlreadyPending,
                target_scene_id,
                requested_tick,
                "a scene transition is already pending",
            ));
        }
        self.pending = Some(SceneTransitionRequest {
            target_scene_id,
            requested_tick,
        });
        Ok(())
    }

    /// Applies a pending transition at exactly the following tick using already validated,
    /// loaded project scenes. On failure the request remains pending and the current scene is
    /// unchanged, allowing the host to report diagnostics or retry after loading is repaired.
    ///
    /// # Errors
    ///
    /// Returns [`SceneTransitionError`] for non-contiguous timing, a missing target scene, or a
    /// target whose ID does not agree with its catalog key.
    pub fn apply(
        &mut self,
        tick: u64,
        scenes: &[(String, SceneDocument)],
    ) -> Result<Option<SceneDocument>, SceneTransitionError> {
        let Some(request) = self.pending.as_ref() else {
            return Ok(None);
        };
        let Some(expected_tick) = request.requested_tick.checked_add(1) else {
            return Err(SceneTransitionError::new(
                SceneTransitionErrorKind::TickOverflow,
                request.target_scene_id.clone(),
                tick,
                "scene transition request tick overflowed",
            ));
        };
        if tick != expected_tick {
            return Err(SceneTransitionError::new(
                SceneTransitionErrorKind::WrongTick,
                request.target_scene_id.clone(),
                tick,
                format!("transition must apply on tick {expected_tick}"),
            ));
        }
        let Some((catalog_id, scene)) = scenes
            .iter()
            .find(|(catalog_id, _)| catalog_id == &request.target_scene_id)
        else {
            return Err(SceneTransitionError::new(
                SceneTransitionErrorKind::MissingScene,
                request.target_scene_id.clone(),
                tick,
                "target scene is not present in the validated scene catalog",
            ));
        };
        if scene.id() != catalog_id {
            return Err(SceneTransitionError::new(
                SceneTransitionErrorKind::InvalidCatalog,
                request.target_scene_id.clone(),
                tick,
                "scene catalog key does not match the scene document UUID",
            ));
        }
        let scene = scene.clone();
        self.current_scene_id.clone_from(catalog_id);
        self.pending = None;
        Ok(Some(scene))
    }
}

/// Stable scene-transition error category.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneTransitionErrorKind {
    /// The target is empty or already current.
    InvalidTarget,
    /// Another transition request is pending.
    AlreadyPending,
    /// The apply tick is not exactly the tick after the request.
    WrongTick,
    /// The target UUID is absent from the loaded scene catalog.
    MissingScene,
    /// The scene catalog key disagrees with its document.
    InvalidCatalog,
    /// The request tick cannot be incremented.
    TickOverflow,
}

/// Inspectable scene-transition failure; missing scene targets remain queued for retry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SceneTransitionError {
    kind: SceneTransitionErrorKind,
    target_scene_id: String,
    tick: u64,
    message: String,
}

impl SceneTransitionError {
    fn new(
        kind: SceneTransitionErrorKind,
        target_scene_id: String,
        tick: u64,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            target_scene_id,
            tick,
            message: message.into(),
        }
    }

    /// Stable error category.
    #[must_use]
    pub const fn kind(&self) -> SceneTransitionErrorKind {
        self.kind
    }

    /// Target scene UUID involved in the failed transition.
    #[must_use]
    pub fn target_scene_id(&self) -> &str {
        &self.target_scene_id
    }

    /// Tick at which transition application was attempted or requested.
    #[must_use]
    pub const fn tick(&self) -> u64 {
        self.tick
    }

    /// Human-readable diagnostic.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for SceneTransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} at tick {} (target {})",
            self.message, self.tick, self.target_scene_id
        )
    }
}

impl Error for SceneTransitionError {}

#[cfg(test)]
mod tests {
    use super::{
        AnimationClipDocument, AnimationEvent, AnimationPlayer, SceneTransitionErrorKind,
        SceneTransitionQueue,
    };
    use hycel_project::SceneDocument;

    const CLIP: &[u8] = br#"{"schema_version":1,"name":"Blink","looping":false,"frames":[{"texture_id":"30000000-0000-4000-8000-000000000001","duration_ticks":1},{"texture_id":"30000000-0000-4000-8000-000000000002","duration_ticks":2}]}"#;
    const SCENE_A: &[u8] = br#"{"schema_version":2,"id":"10000000-0000-4000-8000-000000000001","name":"A","entities":[]}"#;
    const SCENE_B: &[u8] = br#"{"schema_version":2,"id":"10000000-0000-4000-8000-000000000002","name":"B","entities":[]}"#;

    fn scenes() -> Vec<(String, SceneDocument)> {
        [SCENE_A, SCENE_B]
            .into_iter()
            .map(|json| {
                let scene = SceneDocument::parse_json(json, "scenes/test.json").unwrap();
                (scene.id().to_owned(), scene)
            })
            .collect()
    }

    #[test]
    fn animation_playback_uses_ticks_and_emits_completion_once() {
        let clip = AnimationClipDocument::parse_json(CLIP, "assets/blink.animation.json").unwrap();
        let mut player = AnimationPlayer::new(clip.clone());
        assert_eq!(player.texture_id(), "30000000-0000-4000-8000-000000000001");
        assert_eq!(player.advance(4).unwrap(), None);
        assert_eq!(player.advance(5).unwrap(), None);
        assert_eq!(player.frame_index(), 1);
        assert_eq!(player.texture_id(), "30000000-0000-4000-8000-000000000002");
        assert_eq!(player.advance(6).unwrap(), None);
        assert_eq!(player.advance(7).unwrap(), Some(AnimationEvent::Finished));
        assert!(player.is_finished());
        assert_eq!(player.advance(8).unwrap(), None);
        let mut replay = AnimationPlayer::new(clip);
        let replay_events: Vec<_> = [4, 5, 6, 7]
            .map(|tick| replay.advance(tick).unwrap())
            .into_iter()
            .flatten()
            .collect();
        assert_eq!(replay_events, [AnimationEvent::Finished]);
    }

    #[test]
    fn looping_animation_wraps_and_invalid_ticks_do_not_mutate_state() {
        let looping = br#"{"schema_version":1,"name":"Loop","looping":true,"frames":[{"texture_id":"30000000-0000-4000-8000-000000000001","duration_ticks":1}]}"#;
        let mut player =
            AnimationPlayer::new(AnimationClipDocument::parse_json(looping, "clip.json").unwrap());
        player.advance(0).unwrap();
        assert_eq!(player.advance(2).unwrap_err().expected_tick(), 1);
        assert_eq!(player.frame_index(), 0);
        assert_eq!(player.advance(1).unwrap(), Some(AnimationEvent::Looped));
    }

    #[test]
    fn scene_transition_is_tick_boundary_applied_and_reports_missing_targets() {
        let scenes = scenes();
        let mut queue = SceneTransitionQueue::new("10000000-0000-4000-8000-000000000001");
        queue
            .request("10000000-0000-4000-8000-000000000002", 8)
            .unwrap();
        assert_eq!(
            queue.apply(8, &scenes).unwrap_err().kind(),
            SceneTransitionErrorKind::WrongTick
        );
        let transitioned = queue.apply(9, &scenes).unwrap().unwrap();
        assert_eq!(transitioned.id(), "10000000-0000-4000-8000-000000000002");
        assert_eq!(queue.current_scene_id(), transitioned.id());

        queue
            .request("10000000-0000-4000-8000-000000000003", 10)
            .unwrap();
        let error = queue.apply(11, &scenes).unwrap_err();
        assert_eq!(error.kind(), SceneTransitionErrorKind::MissingScene);
        assert_eq!(
            error.target_scene_id(),
            "10000000-0000-4000-8000-000000000003"
        );
        assert_eq!(
            queue.current_scene_id(),
            "10000000-0000-4000-8000-000000000002"
        );
    }
}
