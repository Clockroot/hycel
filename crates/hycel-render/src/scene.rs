use std::ops::Range;

use super::{RenderError, RenderErrorKind};

const MAX_TEXTURE_DIMENSION: u32 = 16_384;
const MAX_TEXTURE_BYTES: usize = 64 * 1024 * 1024;
const MAX_SPRITES_PER_FRAME: usize = 100_000;
const MAX_DEBUG_ITEMS: usize = 128;
const MAX_DEBUG_TEXT_BYTES: usize = 512;
const MAX_DEBUG_GLYPHS_PER_FRAME: usize = 8_192;

/// Renderer-local handle for an uploaded RGBA texture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TextureId(pub(crate) u32);

impl TextureId {
    /// Built-in one-pixel white texture for solid-color sprites and debug glyphs.
    pub const WHITE: Self = Self(0);

    /// Returns the stable index of this texture within its renderer.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// Decoded, tightly packed, top-to-bottom 8-bit RGBA image data for GPU upload.
///
/// Image file decoding is intentionally outside this crate; callers can decode supported formats
/// in asset/import code, then upload the validated pixels with [`super::Renderer::load_texture`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaImage {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl RgbaImage {
    /// Creates an RGBA image after validating dimensions and the exact pixel byte count.
    ///
    /// # Errors
    ///
    /// Returns an error for zero/oversized dimensions, byte-count mismatch, or more than 64 MiB
    /// of pixel data.
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, RenderError> {
        let byte_count = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| invalid_texture("RGBA image byte count overflows this platform"))?;
        if width == 0
            || height == 0
            || width > MAX_TEXTURE_DIMENSION
            || height > MAX_TEXTURE_DIMENSION
            || byte_count > MAX_TEXTURE_BYTES
            || pixels.len() != byte_count
        {
            return Err(invalid_texture(format!(
                "RGBA image dimensions/bytes are invalid (got {width}x{height}, {} bytes; expected {byte_count}, maximum {MAX_TEXTURE_BYTES})",
                pixels.len()
            )));
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Returns the image width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Returns the image height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    pub(crate) const fn byte_len(&self) -> usize {
        self.pixels.len()
    }

    pub(crate) fn into_parts(self) -> (u32, u32, Vec<u8>) {
        (self.width, self.height, self.pixels)
    }
}

/// A camera-projected textured rectangle. Positions/sizes use presentation world units (+Y down).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sprite {
    /// Renderer-local texture handle returned from `Renderer::load_texture`.
    pub texture: TextureId,
    /// World-space center position; positive Y points down.
    pub center: [f32; 2],
    /// Positive world-space width and height.
    pub size: [f32; 2],
    /// Lower values are drawn first.
    pub layer: i32,
    /// Lower values are drawn first within a layer.
    pub order: i32,
    /// Linear RGBA tint multiplied by the sampled texture; each channel must be in `0..=1`.
    pub tint: [f32; 4],
}

impl Sprite {
    /// Creates a white-tinted sprite on layer/order zero.
    #[must_use]
    pub const fn new(texture: TextureId, center: [f32; 2], size: [f32; 2]) -> Self {
        Self {
            texture,
            center,
            size,
            layer: 0,
            order: 0,
            tint: [1.0; 4],
        }
    }
}

/// Screen-space bitmap text drawn after all sprites as a simple debug overlay.
#[derive(Clone, Debug, PartialEq)]
pub struct DebugText {
    /// Top-left screen-space position in physical pixels.
    pub position: [u32; 2],
    /// Text to draw. ASCII letters/digits and a small punctuation set are supported; other glyphs
    /// render as `?`. Newlines start another line.
    pub text: String,
    /// Pixel scale from 1 through 8.
    pub scale: u8,
    /// Linear RGBA glyph color; each channel must be in `0..=1`.
    pub color: [f32; 4],
}

impl DebugText {
    /// Creates a one-pixel, white debug label.
    #[must_use]
    pub fn new(position: [u32; 2], text: impl Into<String>) -> Self {
        Self {
            position,
            text: text.into(),
            scale: 1,
            color: [1.0; 4],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Vertex {
    pub(crate) position: [f32; 2],
    pub(crate) uv: [f32; 2],
    pub(crate) color: [f32; 4],
    pub(crate) screen_space: f32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DrawBatch {
    pub(crate) texture: TextureId,
    pub(crate) vertices: Range<u32>,
}

#[derive(Debug)]
pub(crate) struct SceneGeometry {
    pub(crate) sprite_vertices: Vec<Vertex>,
    pub(crate) batches: Vec<DrawBatch>,
    pub(crate) overlay_vertices: Vec<Vertex>,
}

pub(crate) fn prepare_scene(
    sprites: &[Sprite],
    debug_items: &[DebugText],
) -> Result<SceneGeometry, RenderError> {
    if sprites.len() > MAX_SPRITES_PER_FRAME || debug_items.len() > MAX_DEBUG_ITEMS {
        return Err(RenderError::new(
            RenderErrorKind::InvalidScene,
            "scene exceeds the per-frame sprite or debug-text limit",
        ));
    }
    let mut sorted: Vec<_> = sprites.iter().enumerate().collect();
    sorted.sort_by_key(|(index, sprite)| (sprite.layer, sprite.order, *index));
    let mut sprite_vertices = Vec::with_capacity(sprites.len().saturating_mul(6));
    let mut batches: Vec<DrawBatch> = Vec::new();
    for (_, sprite) in sorted {
        validate_sprite(sprite)?;
        let start = u32::try_from(sprite_vertices.len()).map_err(|_| {
            RenderError::new(RenderErrorKind::InvalidScene, "scene vertex count overflow")
        })?;
        push_quad(
            &mut sprite_vertices,
            sprite.center,
            sprite.size,
            sprite.tint,
            false,
        )?;
        let end = u32::try_from(sprite_vertices.len()).map_err(|_| {
            RenderError::new(RenderErrorKind::InvalidScene, "scene vertex count overflow")
        })?;
        if let Some(batch) = batches
            .last_mut()
            .filter(|batch| batch.texture == sprite.texture)
        {
            batch.vertices.end = end;
        } else {
            batches.push(DrawBatch {
                texture: sprite.texture,
                vertices: start..end,
            });
        }
    }
    let overlay_vertices = prepare_debug_text(debug_items)?;
    Ok(SceneGeometry {
        sprite_vertices,
        batches,
        overlay_vertices,
    })
}

fn validate_sprite(sprite: &Sprite) -> Result<(), RenderError> {
    if !sprite.center.iter().all(|value| value.is_finite())
        || !sprite
            .size
            .iter()
            .all(|value| value.is_finite() && *value > 0.0)
        || !valid_color(sprite.tint)
    {
        return Err(RenderError::new(
            RenderErrorKind::InvalidScene,
            "sprite position/size/tint contains invalid values",
        ));
    }
    Ok(())
}

fn prepare_debug_text(items: &[DebugText]) -> Result<Vec<Vertex>, RenderError> {
    let mut glyph_count = 0_usize;
    for item in items {
        if item.text.len() > MAX_DEBUG_TEXT_BYTES
            || !(1..=8).contains(&item.scale)
            || !valid_color(item.color)
        {
            return Err(RenderError::new(
                RenderErrorKind::InvalidScene,
                "debug text exceeds a size, scale, or color limit",
            ));
        }
        glyph_count =
            glyph_count.saturating_add(item.text.chars().filter(|ch| *ch != '\n').count());
        if glyph_count > MAX_DEBUG_GLYPHS_PER_FRAME {
            return Err(RenderError::new(
                RenderErrorKind::InvalidScene,
                "debug overlay exceeds the per-frame glyph limit",
            ));
        }
    }
    let mut vertices = Vec::new();
    for item in items {
        let scale = u32::from(item.scale);
        let origin_x = checked_pixel_add(item.position[0], 0)?;
        let mut x = origin_x;
        let mut y = checked_pixel_add(item.position[1], 0)?;
        for character in item.text.chars() {
            if character == '\n' {
                x = origin_x;
                y = checked_pixel_add(y, 9 * scale)?;
                continue;
            }
            let rows = glyph_rows(character);
            for (row_index, row) in rows.iter().enumerate() {
                let row_offset = u32::try_from(row_index).expect("font row index fits u32");
                for column in 0_u32..5 {
                    if row & (1 << (4 - column)) != 0 {
                        let pixel_x = checked_pixel_add(x, column * scale)?;
                        let pixel_y = checked_pixel_add(y, row_offset * scale)?;
                        let center = [
                            f32::from(
                                u16::try_from(pixel_x).expect("pixel coordinate was checked"),
                            ) + f32::from(item.scale) * 0.5,
                            f32::from(
                                u16::try_from(pixel_y).expect("pixel coordinate was checked"),
                            ) + f32::from(item.scale) * 0.5,
                        ];
                        push_quad(
                            &mut vertices,
                            center,
                            [f32::from(item.scale); 2],
                            item.color,
                            true,
                        )?;
                    }
                }
            }
            x = checked_pixel_add(x, 6 * scale)?;
        }
    }
    Ok(vertices)
}

fn checked_pixel_add(pixel: u32, offset: u32) -> Result<u32, RenderError> {
    pixel
        .checked_add(offset)
        .filter(|value| u16::try_from(*value).is_ok())
        .ok_or_else(|| {
            RenderError::new(
                RenderErrorKind::InvalidScene,
                "debug overlay coordinates exceed the exact viewport range",
            )
        })
}

fn push_quad(
    vertices: &mut Vec<Vertex>,
    center: [f32; 2],
    size: [f32; 2],
    color: [f32; 4],
    screen_space: bool,
) -> Result<(), RenderError> {
    let half_width = size[0] * 0.5;
    let half_height = size[1] * 0.5;
    let left = center[0] - half_width;
    let right = center[0] + half_width;
    let top = center[1] - half_height;
    let bottom = center[1] + half_height;
    if ![left, right, top, bottom]
        .iter()
        .all(|value| value.is_finite())
    {
        return Err(RenderError::new(
            RenderErrorKind::InvalidScene,
            "sprite or debug glyph bounds overflow the finite presentation range",
        ));
    }
    let screen_flag = f32::from(u8::from(screen_space));
    let mut push = |position, uv| {
        vertices.push(Vertex {
            position,
            uv,
            color,
            screen_space: screen_flag,
        });
    };
    push([left, top], [0.0, 0.0]);
    push([right, top], [1.0, 0.0]);
    push([right, bottom], [1.0, 1.0]);
    push([left, top], [0.0, 0.0]);
    push([right, bottom], [1.0, 1.0]);
    push([left, bottom], [0.0, 1.0]);
    Ok(())
}

fn valid_color(color: [f32; 4]) -> bool {
    color
        .iter()
        .all(|component| component.is_finite() && (0.0..=1.0).contains(component))
}

fn invalid_texture(message: impl Into<String>) -> RenderError {
    RenderError::new(RenderErrorKind::InvalidTextureData, message)
}

fn glyph_rows(character: char) -> [u8; 7] {
    match character.to_ascii_uppercase() {
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'D' => [30, 17, 17, 17, 17, 17, 30],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'F' => [31, 16, 16, 30, 16, 16, 16],
        'G' => [14, 17, 16, 23, 17, 17, 15],
        'H' => [17, 17, 17, 31, 17, 17, 17],
        'I' => [14, 4, 4, 4, 4, 4, 14],
        'J' => [7, 2, 2, 2, 18, 18, 12],
        'K' => [17, 18, 20, 24, 20, 18, 17],
        'L' => [16, 16, 16, 16, 16, 16, 31],
        'M' => [17, 27, 21, 21, 17, 17, 17],
        'N' => [17, 25, 21, 19, 17, 17, 17],
        'O' => [14, 17, 17, 17, 17, 17, 14],
        'P' => [30, 17, 17, 30, 16, 16, 16],
        'Q' => [14, 17, 17, 17, 21, 18, 13],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'S' => [15, 16, 16, 14, 1, 1, 30],
        'T' => [31, 4, 4, 4, 4, 4, 4],
        'U' => [17, 17, 17, 17, 17, 17, 14],
        'V' => [17, 17, 17, 17, 17, 10, 4],
        'W' => [17, 17, 17, 21, 21, 21, 10],
        'X' => [17, 17, 10, 4, 10, 17, 17],
        'Y' => [17, 17, 10, 4, 4, 4, 4],
        'Z' => [31, 1, 2, 4, 8, 16, 31],
        '0' => [14, 17, 19, 21, 25, 17, 14],
        '1' => [4, 12, 4, 4, 4, 4, 14],
        '2' => [14, 17, 1, 2, 4, 8, 31],
        '3' => [30, 1, 1, 14, 1, 1, 30],
        '4' => [2, 6, 10, 18, 31, 2, 2],
        '5' => [31, 16, 16, 30, 1, 1, 30],
        '6' => [14, 16, 16, 30, 17, 17, 14],
        '7' => [31, 1, 2, 4, 8, 8, 8],
        '8' => [14, 17, 17, 14, 17, 17, 14],
        '9' => [14, 17, 17, 15, 1, 1, 14],
        ':' => [0, 4, 4, 0, 4, 4, 0],
        '.' => [0, 0, 0, 0, 0, 4, 4],
        ',' => [0, 0, 0, 0, 4, 4, 8],
        '-' => [0, 0, 0, 31, 0, 0, 0],
        '+' => [0, 4, 4, 31, 4, 4, 0],
        '/' => [1, 2, 2, 4, 8, 8, 16],
        '_' => [0, 0, 0, 0, 0, 0, 31],
        ' ' => [0; 7],
        _ => [14, 17, 1, 2, 4, 0, 4],
    }
}

#[cfg(test)]
mod tests {
    use super::{DebugText, RgbaImage, Sprite, TextureId, prepare_scene};
    use crate::RenderErrorKind;

    #[test]
    fn rgba_image_rejects_wrong_byte_counts_and_accepts_valid_pixel_data() {
        assert_eq!(
            RgbaImage::new(2, 2, vec![0; 15]).unwrap_err().kind(),
            RenderErrorKind::InvalidTextureData
        );
        assert_eq!(
            RgbaImage::new(0, 1, Vec::new()).unwrap_err().kind(),
            RenderErrorKind::InvalidTextureData
        );
        let image = RgbaImage::new(2, 1, vec![255; 8]).unwrap();
        assert_eq!((image.width(), image.height(), image.byte_len()), (2, 1, 8));
    }

    #[test]
    fn sprites_are_stably_sorted_by_layer_then_order_and_batched_by_texture() {
        let sprites = [
            Sprite {
                layer: 1,
                order: 0,
                ..Sprite::new(TextureId(2), [0.0, 0.0], [1.0, 1.0])
            },
            Sprite {
                layer: 0,
                order: 0,
                ..Sprite::new(TextureId(1), [2.0, 0.0], [1.0, 1.0])
            },
            Sprite {
                layer: 0,
                order: 0,
                ..Sprite::new(TextureId(2), [0.0, 0.0], [1.0, 1.0])
            },
            Sprite {
                layer: 0,
                order: 1,
                ..Sprite::new(TextureId(1), [1.0, 0.0], [1.0, 1.0])
            },
        ];
        let geometry = prepare_scene(&sprites, &[]).unwrap();
        assert_eq!(geometry.sprite_vertices.len(), 24);
        assert_eq!(geometry.batches.len(), 4);
        assert_eq!(geometry.batches[0].texture, TextureId(1));
        assert_eq!(geometry.batches[0].vertices, 0..6);
        assert_eq!(geometry.batches[1].texture, TextureId(2));
        assert_eq!(geometry.batches[2].texture, TextureId(1));
        assert_eq!(geometry.batches[3].texture, TextureId(2));
        let first_position = geometry.sprite_vertices[0].position;
        assert!((first_position[0] - 1.5).abs() < f32::EPSILON);
        assert!((first_position[1] + 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn sprites_reject_non_finite_quad_bounds() {
        let sprite = Sprite::new(TextureId::WHITE, [f32::MAX, 0.0], [f32::MAX, 1.0]);
        assert_eq!(
            prepare_scene(&[sprite], &[]).unwrap_err().kind(),
            RenderErrorKind::InvalidScene
        );
    }

    #[test]
    fn debug_text_builds_screen_space_glyphs_and_rejects_invalid_scale() {
        let label = DebugText::new([10, 12], "A1");
        let geometry = prepare_scene(&[], &[label]).unwrap();
        assert!(!geometry.overlay_vertices.is_empty());
        assert!(
            geometry
                .overlay_vertices
                .iter()
                .all(|vertex| (vertex.screen_space - 1.0).abs() < f32::EPSILON)
        );
        let bad = DebugText {
            scale: 0,
            ..DebugText::new([0, 0], "debug")
        };
        assert_eq!(
            prepare_scene(&[], &[bad]).unwrap_err().kind(),
            RenderErrorKind::InvalidScene
        );
    }
}
