//! Strict, versioned physical-input bindings and deterministic tick-frame mapping.
//!
//! This crate is independent of native windowing. Platform adapters translate supported physical
//! key and mouse-button events into [`InputEvent`]; this mapper converts the current held state into
//! [`hycel_core::InputFrame`] snapshots. Input events and binding configuration never enter the
//! authoritative simulation except through an explicitly tick-indexed frame.

use std::{collections::BTreeSet, error::Error, fmt};

use hycel_core::InputFrame;
use serde::{Deserialize, Serialize};

/// Maximum UTF-8 input-binding document size.
pub const MAX_INPUT_BINDINGS_BYTES: usize = 64 * 1024;
const MAX_BUTTON_ACTIONS: usize = 256;
const MAX_AXIS_ACTIONS: usize = 128;
const MAX_CONTROLS_PER_ACTION: usize = 64;
const MAX_TOTAL_CONTROLS: usize = 512;

/// Stable physical keyboard key names used in project input bindings.
///
/// These are physical positions, not localized text values. Key names intentionally match the
/// conventional `KeyboardEvent.code`/USB usage-style names and do not depend on keyboard layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum KeyCode {
    KeyA,
    KeyB,
    KeyC,
    KeyD,
    KeyE,
    KeyF,
    KeyG,
    KeyH,
    KeyI,
    KeyJ,
    KeyK,
    KeyL,
    KeyM,
    KeyN,
    KeyO,
    KeyP,
    KeyQ,
    KeyR,
    KeyS,
    KeyT,
    KeyU,
    KeyV,
    KeyW,
    KeyX,
    KeyY,
    KeyZ,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
    Space,
    Enter,
    Escape,
    Tab,
    Backspace,
    LeftShift,
    RightShift,
    LeftControl,
    RightControl,
    LeftAlt,
    RightAlt,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

/// Stable mouse button names used in project input bindings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

/// One physical control that can activate an action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputControl {
    /// A physical keyboard key.
    Key {
        /// Stable physical key identifier.
        code: KeyCode,
    },
    /// A mouse button.
    MouseButton {
        /// Stable mouse button identifier.
        button: MouseButton,
    },
}

/// Focus-loss behavior stored with the project's input bindings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusLossBehavior {
    /// Clear all held keys/buttons on focus loss, preventing stuck movement/actions.
    #[default]
    ReleaseAll,
    /// Preserve the held state on focus loss. Select only when the host intentionally wants
    /// background input to remain active.
    PreserveHeld,
}

/// One digital action and its alternative physical controls.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ButtonBinding {
    action_id: u16,
    controls: Vec<InputControl>,
}

impl ButtonBinding {
    /// Creates one button action binding. A control press sets this action true.
    #[must_use]
    pub fn new(action_id: u16, controls: Vec<InputControl>) -> Self {
        Self {
            action_id,
            controls,
        }
    }

    /// Numeric action ID stored in [`InputFrame`].
    #[must_use]
    pub const fn action_id(&self) -> u16 {
        self.action_id
    }

    /// Alternative controls that activate this action.
    #[must_use]
    pub fn controls(&self) -> &[InputControl] {
        &self.controls
    }
}

/// One signed digital axis and its negative/positive physical controls.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AxisBinding {
    action_id: u16,
    negative: Vec<InputControl>,
    positive: Vec<InputControl>,
}

impl AxisBinding {
    /// Creates an axis binding. Negative-only input maps to `i16::MIN`, positive-only to
    /// `i16::MAX`, and simultaneous opposing input cancels to zero.
    #[must_use]
    pub fn new(action_id: u16, negative: Vec<InputControl>, positive: Vec<InputControl>) -> Self {
        Self {
            action_id,
            negative,
            positive,
        }
    }

    /// Numeric axis action ID stored in [`InputFrame`].
    #[must_use]
    pub const fn action_id(&self) -> u16 {
        self.action_id
    }

    /// Controls that contribute the negative axis value.
    #[must_use]
    pub fn negative_controls(&self) -> &[InputControl] {
        &self.negative
    }

    /// Controls that contribute the positive axis value.
    #[must_use]
    pub fn positive_controls(&self) -> &[InputControl] {
        &self.positive
    }
}

/// Validated version-1 input-bindings document, normally stored as `input.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InputBindings {
    schema_version: u32,
    focus_loss: FocusLossBehavior,
    buttons: Vec<ButtonBinding>,
    axes: Vec<AxisBinding>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputBindingsWire {
    schema_version: u32,
    focus_loss: FocusLossBehavior,
    buttons: Vec<ButtonBinding>,
    axes: Vec<AxisBinding>,
}

impl Default for InputBindings {
    fn default() -> Self {
        Self {
            schema_version: 1,
            focus_loss: FocusLossBehavior::ReleaseAll,
            buttons: Vec::new(),
            axes: Vec::new(),
        }
    }
}

impl InputBindings {
    /// Creates and validates a version-1 action mapping.
    ///
    /// # Errors
    ///
    /// Returns [`InputBindingsError`] when action IDs, controls, or configured resource limits are invalid.
    pub fn new(
        focus_loss: FocusLossBehavior,
        buttons: Vec<ButtonBinding>,
        axes: Vec<AxisBinding>,
    ) -> Result<Self, InputBindingsError> {
        let bindings = Self {
            schema_version: 1,
            focus_loss,
            buttons,
            axes,
        };
        bindings.validate()?;
        Ok(bindings)
    }

    /// Parses a bounded JSON input-bindings document and validates duplicate IDs and controls.
    ///
    /// # Errors
    ///
    /// Returns [`InputBindingsError`] for oversized, malformed, unsupported, or semantically invalid data.
    pub fn parse_json(input: &[u8]) -> Result<Self, InputBindingsError> {
        if input.len() > MAX_INPUT_BINDINGS_BYTES {
            return Err(InputBindingsError::new(
                InputBindingsErrorKind::TooLarge,
                "$",
                format!("input bindings exceed the {MAX_INPUT_BINDINGS_BYTES}-byte limit"),
            ));
        }
        let wire: InputBindingsWire = serde_json::from_slice(input).map_err(|error| {
            InputBindingsError::new(
                InputBindingsErrorKind::InvalidDocument,
                "$",
                format!("invalid or unknown input-binding fields: {error}"),
            )
        })?;
        if wire.schema_version != 1 {
            return Err(InputBindingsError::new(
                InputBindingsErrorKind::UnsupportedVersion,
                "$.schema_version",
                "unsupported input-bindings schema version",
            ));
        }
        Self::new(wire.focus_loss, wire.buttons, wire.axes)
    }

    /// Serializes the validated document as deterministic, pretty-printed JSON with a final LF.
    ///
    /// # Errors
    ///
    /// Returns a serde JSON error if serialization fails.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        let mut json = serde_json::to_string_pretty(self)?;
        json.push('\n');
        Ok(json)
    }

    fn validate(&self) -> Result<(), InputBindingsError> {
        if self.schema_version != 1 {
            return Err(InputBindingsError::new(
                InputBindingsErrorKind::UnsupportedVersion,
                "$.schema_version",
                "unsupported input-bindings schema version",
            ));
        }
        if self.buttons.len() > MAX_BUTTON_ACTIONS || self.axes.len() > MAX_AXIS_ACTIONS {
            return Err(InputBindingsError::new(
                InputBindingsErrorKind::InvalidBinding,
                "$",
                format!(
                    "input bindings exceed {MAX_BUTTON_ACTIONS} button or {MAX_AXIS_ACTIONS} axis actions"
                ),
            ));
        }
        let mut action_ids = BTreeSet::new();
        let mut total_controls = 0_usize;
        for (index, binding) in self.buttons.iter().enumerate() {
            let path = format!("$.buttons[{index}]");
            if !action_ids.insert(binding.action_id) {
                return Err(InputBindingsError::new(
                    InputBindingsErrorKind::InvalidBinding,
                    format!("{path}.action_id"),
                    "action IDs must be unique across button and axis bindings",
                ));
            }
            validate_controls(&binding.controls, &format!("{path}.controls"), false)?;
            total_controls = total_controls.saturating_add(binding.controls.len());
        }
        for (index, binding) in self.axes.iter().enumerate() {
            let path = format!("$.axes[{index}]");
            if !action_ids.insert(binding.action_id) {
                return Err(InputBindingsError::new(
                    InputBindingsErrorKind::InvalidBinding,
                    format!("{path}.action_id"),
                    "action IDs must be unique across button and axis bindings",
                ));
            }
            if binding.negative.is_empty() && binding.positive.is_empty() {
                return Err(InputBindingsError::new(
                    InputBindingsErrorKind::InvalidBinding,
                    path,
                    "an axis must bind at least one physical control",
                ));
            }
            validate_controls(&binding.negative, &format!("{path}.negative"), true)?;
            validate_controls(&binding.positive, &format!("{path}.positive"), true)?;
            total_controls = total_controls
                .saturating_add(binding.negative.len())
                .saturating_add(binding.positive.len());
            if binding
                .negative
                .iter()
                .any(|control| binding.positive.contains(control))
            {
                return Err(InputBindingsError::new(
                    InputBindingsErrorKind::InvalidBinding,
                    path,
                    "the same physical control cannot be both negative and positive on one axis",
                ));
            }
        }
        if total_controls > MAX_TOTAL_CONTROLS {
            return Err(InputBindingsError::new(
                InputBindingsErrorKind::InvalidBinding,
                "$",
                format!("input bindings exceed the {MAX_TOTAL_CONTROLS}-control total limit"),
            ));
        }
        Ok(())
    }

    /// Input-bindings schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Configured focus-loss behavior.
    #[must_use]
    pub const fn focus_loss_behavior(&self) -> FocusLossBehavior {
        self.focus_loss
    }

    /// Digital action bindings.
    #[must_use]
    pub fn buttons(&self) -> &[ButtonBinding] {
        &self.buttons
    }

    /// Signed axis action bindings.
    #[must_use]
    pub fn axes(&self) -> &[AxisBinding] {
        &self.axes
    }
}

fn validate_controls(
    controls: &[InputControl],
    path: &str,
    may_be_empty: bool,
) -> Result<(), InputBindingsError> {
    if (!may_be_empty && controls.is_empty()) || controls.len() > MAX_CONTROLS_PER_ACTION {
        return Err(InputBindingsError::new(
            InputBindingsErrorKind::InvalidBinding,
            path,
            format!("bind 1..={MAX_CONTROLS_PER_ACTION} controls for this action"),
        ));
    }
    let mut unique = BTreeSet::new();
    for control in controls {
        if !unique.insert(*control) {
            return Err(InputBindingsError::new(
                InputBindingsErrorKind::InvalidBinding,
                path,
                "duplicate physical control in one action binding",
            ));
        }
    }
    Ok(())
}

/// One normalized state change from a physical key or mouse button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// A physical key changed to pressed/released.
    Key {
        code: KeyCode,
        pressed: bool,
        /// Whether the window system synthesized this focus-transition event.
        synthetic: bool,
    },
    /// A mouse button changed to pressed/released.
    MouseButton { button: MouseButton, pressed: bool },
}

/// Stateful translator from normalized physical events to deterministic tick snapshots.
#[derive(Clone, Debug)]
pub struct InputMapper {
    bindings: InputBindings,
    keys_down: BTreeSet<KeyCode>,
    mouse_buttons_down: BTreeSet<MouseButton>,
}

impl InputMapper {
    /// Creates a mapper from validated bindings.
    #[must_use]
    pub fn new(bindings: InputBindings) -> Self {
        Self {
            bindings,
            keys_down: BTreeSet::new(),
            mouse_buttons_down: BTreeSet::new(),
        }
    }

    /// Applies one normalized physical state change. Repeated identical events are idempotent.
    pub fn handle_event(&mut self, event: InputEvent) {
        match event {
            InputEvent::Key {
                code,
                pressed,
                synthetic,
            } => {
                if !synthetic {
                    update_set(&mut self.keys_down, code, pressed);
                }
            }
            InputEvent::MouseButton { button, pressed } => {
                update_set(&mut self.mouse_buttons_down, button, pressed);
            }
        }
    }

    /// Applies a window focus transition. The configured policy is applied on focus loss only.
    pub fn focus_changed(&mut self, focused: bool) {
        if !focused && self.bindings.focus_loss == FocusLossBehavior::ReleaseAll {
            self.keys_down.clear();
            self.mouse_buttons_down.clear();
        }
    }

    /// Builds an immutable action snapshot for exactly `tick`.
    #[must_use]
    pub fn frame(&self, tick: u64) -> InputFrame {
        let mut frame = InputFrame::new(tick);
        for binding in &self.bindings.buttons {
            if binding
                .controls
                .iter()
                .any(|control| self.is_down(*control))
            {
                frame.set_button(binding.action_id, true);
            }
        }
        for binding in &self.bindings.axes {
            let negative = binding
                .negative
                .iter()
                .any(|control| self.is_down(*control));
            let positive = binding
                .positive
                .iter()
                .any(|control| self.is_down(*control));
            let value = match (negative, positive) {
                (true, false) => i16::MIN,
                (false, true) => i16::MAX,
                (true, true) | (false, false) => 0,
            };
            frame.set_axis(binding.action_id, value);
        }
        frame
    }

    fn is_down(&self, control: InputControl) -> bool {
        match control {
            InputControl::Key { code } => self.keys_down.contains(&code),
            InputControl::MouseButton { button } => self.mouse_buttons_down.contains(&button),
        }
    }
}

fn update_set<T: Ord>(set: &mut BTreeSet<T>, value: T, pressed: bool) {
    if pressed {
        set.insert(value);
    } else {
        set.remove(&value);
    }
}

/// Stable failure kind for parsing or validating persisted input bindings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputBindingsErrorKind {
    /// The file exceeds the configured parser bound.
    TooLarge,
    /// JSON is malformed or contains an unknown/duplicate field.
    InvalidDocument,
    /// The schema version is not supported.
    UnsupportedVersion,
    /// A binding is empty, duplicated, or otherwise inconsistent.
    InvalidBinding,
}

/// Structured error for user-authored input bindings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputBindingsError {
    kind: InputBindingsErrorKind,
    path: String,
    message: String,
}

impl InputBindingsError {
    fn new(
        kind: InputBindingsErrorKind,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            path: path.into(),
            message: message.into(),
        }
    }

    /// Stable failure category.
    #[must_use]
    pub const fn kind(&self) -> InputBindingsErrorKind {
        self.kind
    }

    /// JSONPath-like binding location.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Human-readable explanation.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for InputBindingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {} ({:?})", self.path, self.message, self.kind)
    }
}

impl Error for InputBindingsError {}

#[cfg(test)]
mod tests {
    use super::{
        AxisBinding, ButtonBinding, FocusLossBehavior, InputBindings, InputBindingsErrorKind,
        InputControl, InputEvent, InputMapper, KeyCode, MouseButton,
    };

    fn key(code: KeyCode) -> InputControl {
        InputControl::Key { code }
    }

    fn mouse(button: MouseButton) -> InputControl {
        InputControl::MouseButton { button }
    }

    fn bindings(focus_loss: FocusLossBehavior) -> InputBindings {
        InputBindings::new(
            focus_loss,
            vec![ButtonBinding::new(
                1,
                vec![key(KeyCode::Space), mouse(MouseButton::Left)],
            )],
            vec![AxisBinding::new(
                2,
                vec![key(KeyCode::ArrowLeft)],
                vec![key(KeyCode::ArrowRight)],
            )],
        )
        .unwrap()
    }

    #[test]
    fn bindings_round_trip_as_strict_versioned_json() {
        let bindings = bindings(FocusLossBehavior::ReleaseAll);
        let json = bindings.to_json().unwrap();
        assert!(json.ends_with('\n'));
        assert_eq!(
            InputBindings::parse_json(json.as_bytes()).unwrap(),
            bindings
        );
        assert_eq!(bindings.schema_version(), 1);
    }

    #[test]
    fn parser_rejects_unknown_fields_unsupported_versions_and_oversized_input() {
        assert_eq!(
            InputBindings::parse_json(
                br#"{"schema_version":2,"focus_loss":"release_all","buttons":[],"axes":[]}"#
            )
            .unwrap_err()
            .kind(),
            InputBindingsErrorKind::UnsupportedVersion
        );
        assert_eq!(
            InputBindings::parse_json(br#"{"schema_version":1,"focus_loss":"release_all","buttons":[],"axes":[],"extra":true}"#)
                .unwrap_err()
                .kind(),
            InputBindingsErrorKind::InvalidDocument
        );
        assert_eq!(
            InputBindings::parse_json(br#"{"schema_version":1,"schema_version":1,"focus_loss":"release_all","buttons":[],"axes":[]}"#)
                .unwrap_err()
                .kind(),
            InputBindingsErrorKind::InvalidDocument
        );
        assert_eq!(
            InputBindings::parse_json(&vec![b' '; super::MAX_INPUT_BINDINGS_BYTES + 1])
                .unwrap_err()
                .kind(),
            InputBindingsErrorKind::TooLarge
        );
    }

    #[test]
    fn bindings_reject_duplicate_actions_controls_and_axis_directions() {
        assert_eq!(
            InputBindings::new(
                FocusLossBehavior::ReleaseAll,
                vec![ButtonBinding::new(3, vec![key(KeyCode::Space)])],
                vec![AxisBinding::new(3, vec![], vec![key(KeyCode::KeyA)])],
            )
            .unwrap_err()
            .kind(),
            InputBindingsErrorKind::InvalidBinding
        );
        assert!(
            InputBindings::new(
                FocusLossBehavior::ReleaseAll,
                vec![ButtonBinding::new(
                    1,
                    vec![key(KeyCode::Space), key(KeyCode::Space)]
                )],
                vec![],
            )
            .is_err()
        );
        assert!(
            InputBindings::new(
                FocusLossBehavior::ReleaseAll,
                vec![],
                vec![AxisBinding::new(
                    2,
                    vec![key(KeyCode::KeyA)],
                    vec![key(KeyCode::KeyA)],
                )],
            )
            .is_err()
        );
    }

    #[test]
    fn mapper_snapshots_alternative_buttons_axes_and_opposing_inputs_deterministically() {
        let mut mapper = InputMapper::new(bindings(FocusLossBehavior::ReleaseAll));
        mapper.handle_event(InputEvent::Key {
            code: KeyCode::Space,
            pressed: true,
            synthetic: false,
        });
        mapper.handle_event(InputEvent::Key {
            code: KeyCode::ArrowLeft,
            pressed: true,
            synthetic: false,
        });
        assert!(mapper.frame(4).button(1));
        assert_eq!(mapper.frame(4).axis(2), i16::MIN);
        mapper.handle_event(InputEvent::Key {
            code: KeyCode::ArrowRight,
            pressed: true,
            synthetic: false,
        });
        assert_eq!(mapper.frame(5).axis(2), 0);
        mapper.handle_event(InputEvent::Key {
            code: KeyCode::Space,
            pressed: false,
            synthetic: false,
        });
        mapper.handle_event(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        });
        assert!(mapper.frame(6).button(1));
        mapper.handle_event(InputEvent::Key {
            code: KeyCode::ArrowLeft,
            pressed: false,
            synthetic: false,
        });
        assert_eq!(mapper.frame(7).axis(2), i16::MAX);
        assert!(!mapper.frame(7).button(99));
    }

    #[test]
    fn focus_loss_applies_the_persisted_release_or_preserve_policy() {
        let mut releasing = InputMapper::new(bindings(FocusLossBehavior::ReleaseAll));
        releasing.handle_event(InputEvent::Key {
            code: KeyCode::Space,
            pressed: true,
            synthetic: false,
        });
        releasing.focus_changed(false);
        releasing.handle_event(InputEvent::Key {
            code: KeyCode::Space,
            pressed: true,
            synthetic: true,
        });
        releasing.focus_changed(true);
        assert!(!releasing.frame(0).button(1));

        let mut preserving = InputMapper::new(bindings(FocusLossBehavior::PreserveHeld));
        preserving.handle_event(InputEvent::Key {
            code: KeyCode::Space,
            pressed: true,
            synthetic: false,
        });
        preserving.focus_changed(false);
        preserving.handle_event(InputEvent::Key {
            code: KeyCode::Space,
            pressed: false,
            synthetic: true,
        });
        assert!(preserving.frame(0).button(1));
    }
}
