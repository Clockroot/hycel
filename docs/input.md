# Named input actions (schema 2)

Hycel keeps OS events outside `hycel-core`. `hycel-platform` translates supported native keyboard and mouse-button events into Hycel-owned physical controls; `hycel-input::InputMapper` applies the project's mapping and emits explicit tick-indexed `hycel_core::InputFrame` snapshots. The host chooses tick assignment; OS event timing never enters `hycel-core` implicitly.

## Project file

`input.json` is an optional project-root file so existing projects remain valid. `hycel new` creates it; `hycel check` validates it when present. It is strict UTF-8 JSON, schema-versioned independently from the TOML manifest, and bounded to 64 KiB, 256 digital actions, 128 axes, 64 controls per action, and 512 controls total. It rejects unknown/duplicate fields, unsupported versions, duplicate action IDs or names, invalid names, empty button bindings, duplicate controls, and contradictory controls on one axis. File operations use the same project-root containment rules as other authored files.

```json
{
  "schema_version": 2,
  "focus_loss": "release_all",
  "buttons": [
    { "action_id": 1, "name": "jump", "controls": [{ "kind": "key", "code": "Space" }] }
  ],
  "axes": [
    {
      "action_id": 0,
      "name": "move_horizontal",
      "negative": [
        { "kind": "key", "code": "KeyA" },
        { "kind": "key", "code": "ArrowLeft" }
      ],
      "positive": [
        { "kind": "key", "code": "KeyD" },
        { "kind": "key", "code": "ArrowRight" }
      ]
    }
  ]
}
```

Each action has a stable numeric `action_id` and a human-readable machine name. IDs are explicit, unique `u16` values and are stored in `InputFrame` and replay data; names are unique lowercase ASCII identifiers (1–64 characters, starting with a letter; remaining characters may be lowercase letters, digits, `_`, or `-`). Resolve names to stable IDs with `InputBindings::action_id`. Renaming an action does not change its replay ID; changing its ID changes the replayed action channel and should be treated as a game/replay compatibility change. Button names and axis names share one namespace. Multiple controls in one button binding are alternatives (logical OR). Axis input maps to `i16::MIN` or `i16::MAX`; simultaneous negative and positive input cancels to zero. A physical control can intentionally contribute to more than one action.

The parser also accepts strict schema-1 documents and migrates them in memory without changing action IDs or physical bindings. Since schema 1 had no names, migrated entries receive names `action_<id>`; users can rename these in schema 2 while preserving the numeric ID. Serializing a migrated document always writes schema 2. Unsupported versions and unknown fields remain errors.

Keyboard names use stable physical key positions, not localized text: the first supported set is A–Z (`KeyA` … `KeyZ`), 0–9 (`Digit0` … `Digit9`), arrows, Space, Enter, Escape, Tab, Backspace, left/right Shift/Control/Alt, and F1–F12. Supported mouse bindings are Left, Right, Middle, Back, and Forward. Unsupported native keys/buttons are ignored rather than converted to a platform-specific identifier. Text/IME input, mouse motion/wheel, gamepads, and chords/modifiers are not implemented.

`focus_loss` is either `release_all` (the safe default, prevents stuck movement after alt-tab) or `preserve_held` (explicit opt-in for background control). Native key events tagged synthetic by winit during focus transitions are ignored, so they cannot re-press a cleared action or cancel the preserve policy. If actions are released, they remain released after focus returns until new physical press events arrive. Rebinding remains a validated edit to `input.json`; there is no editor or live-rebind UI yet. The Bellglass Courier's HUD reads its movement, jump, checkpoint, and Echo labels from the active authored bindings (including keyboard alternatives and mouse buttons), so those instructions stay accurate when the file is customized.

Button press/release events are derived from consecutive tick-indexed `InputFrame`s by `ActionEventTracker`, not directly from OS events. This keeps event edges reproducible during replay. The tracker requires contiguous frame ticks and emits named events in stable numeric action-ID order. Axis values remain continuous per-frame state. Gameplay code receives only action frames/events and never native key codes.
