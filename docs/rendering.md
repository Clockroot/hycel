# 2D rendering (initial slice)

`hycel-render` is a presentation-only backend. It owns wgpu surface, device, texture, buffer, bind-group, and pipeline objects; game-facing calls use Hycel types. It must never mutate authoritative simulation state.

## Coordinates and camera

`Camera2D` and `Sprite` use presentation world units with +X right and +Y down. Sprite position is its center; size must be positive. The camera center maps to the viewport center, and positive finite zoom magnifies the scene while preserving viewport aspect ratio. Transform inputs that overflow the finite matrix range are rejected. Debug labels use physical screen pixels and are rendered after all sprites, independent of the world camera.

## Textures and sprites

Call `RgbaImage::new(width, height, pixels)` with tightly packed, top-to-bottom RGBA8 rows; byte length must equal `width * height * 4`. The image is CPU-decoded data: image-file parsing/format support remains in asset/import code, not in this renderer. `Renderer::load_texture` uploads it as sRGB and returns a renderer-local `TextureId`. The built-in `TextureId::WHITE` supports solid-color quads and the text overlay. Sampling currently uses nearest filtering.

Each `Sprite` supplies a texture, center, size, integer layer/order, and linear RGBA tint. The renderer draws in stable `(layer, order, original input position)` order and batches adjacent sprites that use the same texture. Alpha uses the standard source-alpha blend. `render_scene` accepts a world camera, sprite slice, and optional debug-text slice; surfaces that are minimized, occluded, timed out, outdated, or lost return non-fatal `FrameOutcome`s as applicable.

The first slice bounds each image to 64 MiB and aggregate uploaded pixels to 256 MiB per renderer; renderer-local IDs are not deleted/reused. Per-frame sprite/debug counts and debug text length are also bounded. Debug text uses an embedded 5x7 uppercase font for ASCII letters/digits and punctuation plus common dot separators, arrows, brackets, parentheses, dashes, and ellipses; unsupported glyphs display as `?` and line breaks are supported. This is a diagnostic overlay, not general typography.

## Example

```rust,ignore
let texture = renderer.load_texture(decoded_rgba_image)?;
let sprites = [
    Sprite {
        layer: 10,
        tint: [1.0, 0.8, 0.7, 1.0],
        ..Sprite::new(texture, [4.0, 2.0], [1.0, 1.0])
    },
];
let overlay = [DebugText::new([12, 12], "PLAYER: OK")];
renderer.render_scene(camera, &sprites, &overlay)?;
```

The expanded renderer smoke exercises shader validation, RGBA upload, camera transform, texture sampling, sprite ordering/tint/alpha, screen-space text, and presentation; CI run 36956532269 passed its 30-frame smokes on hosted Linux/Vulkan, macOS/Metal, and Windows/D3D12 runners. The smoke asserts successful presentation, not pixel-by-pixel output. Hosted CI adapters are virtual/software devices and do not certify physical hardware, minimum operating systems, packaging, or support. Device recovery, image decoding, atlas management, batching optimization, general font shaping, and resource unload remain future work.
