# Input bindings (schema 1)

Hycel keeps OS events outside `hycel-core`. `hycel-platform` translates supported native keyboard and mouse-button events into Hycel-owned physical controls; `hycel-input::InputMapper` applies the project's mapping and emits explicit tick-indexed `hycel_core::InputFrame` snapshots. The input mapper is a host-side adapter: call it with each normalized event, then call `frame(tick)` immediately before the corresponding simulation tick. The host chooses tick assignment; OS event timing never enters `hycel-core` implicitly.

## Project file

`input.json` is an optional project-root file so existing projects remain valid. `hycel new` creates it; `hycel check` validates it when present. It is strict UTF-8 JSON, schema-versioned independently from the TOML manifest, bounded to 64 KiB, 256 digital actions, 128 axes, 64 controls per action, and 512 controls total. It rejects unknown/duplicate fields, unsupported versions, duplicate action IDs, empty button bindings, duplicate controls, and contradictory controls on one axis. File operations use the same project-root containment rules as other authored files.

```json
{
  "schema_version": 1,
  "focus_loss": "release_all",
  "buttons": [
    { "action_id": 1, "controls": [{ "kind": "key", "code": "Space" }] }
  ],
  "axes": [
    {
      "action_id": 0,
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

Action IDs are stable numeric `u16` values shared with `InputFrame`. A project's game code assigns their meaning (the starter file uses action 0 for horizontal movement and action 1 for jump). Multiple controls in one button binding are alternatives (logical OR). Axis input maps to `i16::MIN` or `i16::MAX`; simultaneous negative and positive input cancels to zero. A physical control can intentionally contribute to more than one action.

Keyboard names use stable physical key positions, not localized text: the first supported set is A–Z (`KeyA` … `KeyZ`), 0–9 (`Digit0` … `Digit9`), arrows, Space, Enter, Escape, Tab, Backspace, left/right Shift/Control/Alt, and F1–F12. Supported mouse bindings are Left, Right, Middle, Back, and Forward. Unsupported native keys/buttons are ignored rather than converted to a platform-specific identifier. Text/IME input, mouse motion/wheel, gamepads, chords/modifiers, named action registries, and just-pressed/released edges are not implemented in this phase.

`focus_loss` is either `release_all` (the safe default, prevents stuck movement after alt-tab) or `preserve_held` (explicit opt-in for background control). Native key events tagged synthetic by winit during focus transitions are ignored, so they cannot re-press a cleared action or cancel the preserve policy. If actions are released, they remain released after focus returns until new physical press events arrive. Rebinding is performed by editing the validated `input.json`; there is no editor or live-rebind UI yet. The renderer and deterministic simulation are not coupled to native key codes.
