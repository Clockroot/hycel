//! A minimal safe 2D presentation backend using `wgpu`.
//!
//! The crate owns native graphics objects and exposes only Hycel types. It draws camera-projected
//! textured sprites and a screen-space debug overlay to a [`hycel_platform::WindowHandle`] surface.
//! This initial rendering slice accepts decoded RGBA data; asset decoding and device recovery remain
//! outside its current scope.

use std::{
    error::Error,
    fmt,
    mem::size_of,
    sync::{Arc, Mutex},
};

use hycel_platform::{SurfaceSize, WindowHandle};

mod scene;
pub use scene::{DebugText, RgbaImage, Sprite, TextureId};
use scene::{SceneGeometry, Vertex, prepare_scene};
use wgpu::util::DeviceExt;
use wgpu::{
    Backends, BindGroup, BindGroupDescriptor, BindGroupEntry, BindGroupLayout,
    BindGroupLayoutDescriptor, BindGroupLayoutEntry, BindingResource, BindingType, Buffer,
    BufferBindingType, BufferDescriptor, BufferUsages, Color, ColorTargetState,
    CommandEncoderDescriptor, CurrentSurfaceTexture, Device, DeviceDescriptor, ErrorFilter,
    Extent3d, FilterMode, FragmentState, FrontFace, Instance, InstanceDescriptor, LoadOp,
    Operations, PipelineLayoutDescriptor, PolygonMode, PresentMode, PrimitiveState,
    PrimitiveTopology, Queue, RenderPassColorAttachment, RenderPassDescriptor, RenderPipeline,
    RenderPipelineDescriptor, Sampler, SamplerBindingType, SamplerDescriptor,
    ShaderModuleDescriptor, ShaderSource, StoreOp, Surface, SurfaceColorSpace,
    SurfaceConfiguration, SurfaceTexture, Texture, TextureDescriptor, TextureDimension,
    TextureFormat, TextureSampleType, TextureUsages, TextureView, TextureViewDescriptor,
    VertexBufferLayout, VertexState, VertexStepMode,
};

const SCENE_SHADER: &str = include_str!("shaders/scene.wgsl");
const TEXTURE_PIXELS: [u8; 16] = [
    255, 72, 72, 255, 64, 128, 255, 255, 255, 220, 48, 255, 64, 220, 128, 255,
];

/// A simple camera in presentation world units (+X right, +Y down). Positive zoom magnifies the scene.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera2D {
    /// World-space position at the viewport center.
    pub center: [f32; 2],
    /// Positive finite zoom factor.
    pub zoom: f32,
}

impl Default for Camera2D {
    fn default() -> Self {
        Self {
            center: [0.0, 0.0],
            zoom: 1.0,
        }
    }
}

/// Rendering operation outcome that does not indicate a fatal backend error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameOutcome {
    /// A frame was submitted and presented.
    Presented,
    /// The surface was occluded, timed out, or minimized; retry on a later redraw.
    Skipped,
    /// The surface was reconfigured after an outdated/lost frame; retry on a later redraw.
    Reconfigured,
}

/// Stable category for renderer failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderErrorKind {
    /// No compatible adapter was available for this window surface.
    AdapterUnavailable,
    /// A GPU device could not be created.
    DeviceRequest,
    /// A native presentation surface could not be created or configured.
    Surface,
    /// The surface did not advertise required capabilities.
    SurfaceCapabilities,
    /// The bundled WGSL or render pipeline failed validation.
    ShaderValidation,
    /// The GPU device was lost or reported an uncaptured error.
    DeviceFailure,
    /// A camera contains non-finite coordinates or non-positive zoom.
    InvalidCamera,
    /// Texture data has invalid dimensions or is too large.
    InvalidTextureData,
    /// A sprite refers to a texture not created by this renderer.
    UnknownTexture,
    /// Scene geometry or debug overlay input is invalid or exceeds limits.
    InvalidScene,
}

/// Renderer failure with a stable category and a human-readable diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderError {
    kind: RenderErrorKind,
    message: String,
}

impl RenderError {
    fn new(kind: RenderErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Returns the stable failure category.
    #[must_use]
    pub const fn kind(&self) -> RenderErrorKind {
        self.kind
    }

    /// Returns the diagnostic message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl Error for RenderError {}

struct SurfaceDevice {
    surface: Surface<'static>,
    adapter: wgpu::Adapter,
    device: Device,
    queue: Queue,
}

struct SceneResources {
    pipeline: RenderPipeline,
    camera_buffer: Buffer,
    texture_bind_group_layout: BindGroupLayout,
    textures: Vec<GpuTexture>,
}

struct GpuTexture {
    _texture: Texture,
    _view: TextureView,
    _sampler: Sampler,
    bind_group: BindGroup,
}

/// Owns the GPU surface, device, scene pipeline, and renderer-local texture registry.
///
/// It deliberately has no reference to or mutation access over the authoritative simulation.
pub struct Renderer {
    _window: Arc<WindowHandle>,
    surface: Surface<'static>,
    device: Device,
    queue: Queue,
    config: SurfaceConfiguration,
    suspended: bool,
    pipeline: RenderPipeline,
    camera_buffer: Buffer,
    texture_bind_group_layout: BindGroupLayout,
    textures: Vec<GpuTexture>,
    texture_bytes: usize,
    async_error: Arc<Mutex<Option<RenderError>>>,
}

impl Renderer {
    /// Creates a renderer compatible with a Hycel-owned opaque native window.
    ///
    /// Adapter selection is limited to the platform's planned native backend. This does not force
    /// a software adapter and returns an actionable error if none is available.
    ///
    /// # Errors
    ///
    /// Returns a typed [`RenderError`] for surface, adapter, device, capability, or shader failure.
    pub fn new(window: Arc<WindowHandle>) -> Result<Self, RenderError> {
        let SurfaceDevice {
            surface,
            adapter,
            device,
            queue,
        } = create_surface_device(&window)?;
        let async_error = Arc::new(Mutex::new(None));
        install_device_error_handlers(&device, &async_error);
        let initial_size = window.surface_size();
        let (config, format) = surface_configuration(&surface, &adapter, initial_size)?;
        let resources = create_scene_resources(&device, &queue, format, &async_error)?;
        let suspended = initial_size.width == 0 || initial_size.height == 0;
        if !suspended {
            surface.configure(&device, &config);
        }

        Ok(Self {
            _window: window,
            surface,
            device,
            queue,
            config,
            suspended,
            pipeline: resources.pipeline,
            camera_buffer: resources.camera_buffer,
            texture_bind_group_layout: resources.texture_bind_group_layout,
            textures: resources.textures,
            texture_bytes: 20,
            async_error,
        })
    }

    /// Updates the physical surface dimensions after a native resize event.
    ///
    /// Zero dimensions suspend presentation until a later non-zero resize.
    ///
    /// # Errors
    ///
    /// Returns an error if a device or surface failure has been reported.
    pub fn resize(&mut self, size: SurfaceSize) -> Result<(), RenderError> {
        if size.width == 0 || size.height == 0 {
            self.suspended = true;
            return self.check_async_error();
        }
        validate_surface_dimensions(size)?;
        self.suspended = false;
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
        self.check_async_error()
    }

    /// Clears and draws the demonstration scene, returning a recoverable frame outcome.
    ///
    /// # Errors
    ///
    /// Returns a typed [`RenderError`] for invalid camera state, device failure, or a surface
    /// validation failure. Timeout/occlusion and reconfiguration remain non-fatal outcomes.
    pub fn render(&mut self, camera: Camera2D) -> Result<FrameOutcome, RenderError> {
        let sprites = [
            Sprite {
                tint: [1.0, 0.48, 0.12, 1.0],
                ..Sprite::new(TextureId::WHITE, [-0.52, 0.0], [0.6, 1.1])
            },
            Sprite::new(TextureId(1), [0.52, 0.0], [0.6, 1.1]),
        ];
        self.render_scene(camera, &sprites, &[])
    }

    /// Uploads decoded RGBA pixels and returns a renderer-local texture identifier.
    ///
    /// Image file parsing/decoding is deliberately outside the renderer. Each image is bounded
    /// to 64 MiB, texture dimensions are checked against the device, and the renderer allows at
    /// most 256 MiB of uploaded image data.
    ///
    /// # Errors
    ///
    /// Returns a typed error for texture limits, device validation failures, or exhausted IDs.
    pub fn load_texture(&mut self, image: RgbaImage) -> Result<TextureId, RenderError> {
        const MAX_TEXTURES: usize = 256;
        const MAX_TOTAL_TEXTURE_BYTES: usize = 256 * 1024 * 1024;
        if image.width() > self.device.limits().max_texture_dimension_2d
            || image.height() > self.device.limits().max_texture_dimension_2d
            || self.textures.len() >= MAX_TEXTURES
            || self
                .texture_bytes
                .checked_add(image.byte_len())
                .is_none_or(|bytes| bytes > MAX_TOTAL_TEXTURE_BYTES)
        {
            return Err(RenderError::new(
                RenderErrorKind::InvalidTextureData,
                "image exceeds GPU dimension or renderer texture-memory limits",
            ));
        }
        let id = TextureId(u32::try_from(self.textures.len()).map_err(|_| {
            RenderError::new(
                RenderErrorKind::InvalidTextureData,
                "texture ID space exhausted",
            )
        })?);
        let image_bytes = image.byte_len();
        let scope = self.device.push_error_scope(ErrorFilter::Validation);
        let resource = match create_texture_resource(
            &self.device,
            &self.queue,
            &self.texture_bind_group_layout,
            &self.camera_buffer,
            image,
            "hycel uploaded RGBA texture",
        ) {
            Ok(resource) => resource,
            Err(error) => {
                let _ = pollster::block_on(scope.pop());
                return Err(error);
            }
        };
        if let Some(error) = pollster::block_on(scope.pop()) {
            return Err(RenderError::new(
                RenderErrorKind::DeviceFailure,
                format!("texture upload validation failed: {error}"),
            ));
        }
        self.texture_bytes += image_bytes;
        self.textures.push(resource);
        Ok(id)
    }

    /// Draws sprites in stable `(layer, order, input position)` order, then a screen-space debug
    /// text overlay. Tint and text colors are linear RGBA values.
    ///
    /// # Errors
    ///
    /// Returns a typed error for invalid camera/sprite/text data, unknown texture handles, device
    /// failure, or surface validation failure. Occlusion, timeout, minimized state, and
    /// reconfiguration remain non-fatal frame outcomes.
    pub fn render_scene(
        &mut self,
        camera: Camera2D,
        sprites: &[Sprite],
        debug_text: &[DebugText],
    ) -> Result<FrameOutcome, RenderError> {
        validate_camera(camera)?;
        self.check_async_error()?;
        if self.suspended {
            return Ok(FrameOutcome::Skipped);
        }
        let geometry = prepare_scene(sprites, debug_text)?;
        for batch in &geometry.batches {
            if usize::try_from(batch.texture.index())
                .ok()
                .is_none_or(|index| index >= self.textures.len())
            {
                return Err(RenderError::new(
                    RenderErrorKind::UnknownTexture,
                    format!(
                        "sprite refers to unknown texture ID {}",
                        batch.texture.index()
                    ),
                ));
            }
        }
        let matrix = view_projection(camera, self.config.width, self.config.height)?;
        let overlay_matrix = screen_projection(self.config.width, self.config.height)?;
        validate_projected_vertices(&geometry.sprite_vertices, matrix)?;
        validate_projected_vertices(&geometry.overlay_vertices, overlay_matrix)?;
        self.queue.write_buffer(
            &self.camera_buffer,
            0,
            &matrix_pair_bytes(matrix, overlay_matrix),
        );
        let frame = match self.surface.get_current_texture() {
            CurrentSurfaceTexture::Success(frame) | CurrentSurfaceTexture::Suboptimal(frame) => {
                frame
            }
            CurrentSurfaceTexture::Timeout | CurrentSurfaceTexture::Occluded => {
                return Ok(FrameOutcome::Skipped);
            }
            CurrentSurfaceTexture::Outdated | CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                self.check_async_error()?;
                return Ok(FrameOutcome::Reconfigured);
            }
            CurrentSurfaceTexture::Validation => {
                return Err(RenderError::new(
                    RenderErrorKind::Surface,
                    "surface acquisition reported a validation error",
                ));
            }
        };
        self.draw_frame(frame, &geometry);
        self.check_async_error()?;
        Ok(FrameOutcome::Presented)
    }

    fn draw_frame(&mut self, frame: SurfaceTexture, geometry: &SceneGeometry) {
        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let sprite_buffer = (!geometry.sprite_vertices.is_empty()).then(|| {
            let data = vertex_bytes(&geometry.sprite_vertices);
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("hycel scene sprite vertices"),
                    contents: &data,
                    usage: BufferUsages::VERTEX,
                })
        });
        let overlay_buffer = (!geometry.overlay_vertices.is_empty()).then(|| {
            let data = vertex_bytes(&geometry.overlay_vertices);
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("hycel debug overlay vertices"),
                    contents: &data,
                    usage: BufferUsages::VERTEX,
                })
        });
        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor {
                label: Some("hycel 2d scene encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("hycel 2d scene pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(Color {
                            r: 0.055,
                            g: 0.075,
                            b: 0.12,
                            a: 1.0,
                        }),
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            if let Some(buffer) = &sprite_buffer {
                pass.set_vertex_buffer(0, buffer.slice(..));
                for batch in &geometry.batches {
                    let index = usize::try_from(batch.texture.index())
                        .expect("texture ID was range-checked");
                    pass.set_bind_group(0, &self.textures[index].bind_group, &[]);
                    pass.draw(batch.vertices.clone(), 0..1);
                }
            }
            if let Some(buffer) = &overlay_buffer {
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.set_bind_group(0, &self.textures[0].bind_group, &[]);
                pass.draw(
                    0..u32::try_from(geometry.overlay_vertices.len())
                        .expect("debug vertex count is bounded"),
                    0..1,
                );
            }
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
    }

    fn check_async_error(&self) -> Result<(), RenderError> {
        let guard = self.async_error.lock().map_err(|_| {
            RenderError::new(RenderErrorKind::DeviceFailure, "error state was poisoned")
        })?;
        if let Some(error) = guard.as_ref() {
            return Err(error.clone());
        }
        Ok(())
    }
}

fn create_surface_device(window: &Arc<WindowHandle>) -> Result<SurfaceDevice, RenderError> {
    let mut descriptor = InstanceDescriptor::new_without_display_handle();
    descriptor.backends = native_backend();
    let instance = Instance::new(descriptor);
    let surface = instance
        .create_surface(window.clone())
        .map_err(|error| RenderError::new(RenderErrorKind::Surface, error.to_string()))?;
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        force_fallback_adapter: false,
        compatible_surface: Some(&surface),
        ..Default::default()
    }))
    .map_err(|error| RenderError::new(RenderErrorKind::AdapterUnavailable, error.to_string()))?;
    let info = adapter.get_info();
    eprintln!(
        "hycel-render adapter={} backend={:?} device_type={:?}",
        info.name, info.backend, info.device_type
    );
    let (device, queue) = pollster::block_on(adapter.request_device(&DeviceDescriptor::default()))
        .map_err(|error| RenderError::new(RenderErrorKind::DeviceRequest, error.to_string()))?;
    Ok(SurfaceDevice {
        surface,
        adapter,
        device,
        queue,
    })
}

fn install_device_error_handlers(device: &Device, shared: &Arc<Mutex<Option<RenderError>>>) {
    let uncaptured_error = shared.clone();
    device.on_uncaptured_error(Arc::new(move |error| {
        record_async_error(
            &uncaptured_error,
            RenderError::new(RenderErrorKind::DeviceFailure, error.to_string()),
        );
    }));
    let device_lost_error = shared.clone();
    device.set_device_lost_callback(move |reason, message| {
        record_async_error(
            &device_lost_error,
            RenderError::new(
                RenderErrorKind::DeviceFailure,
                format!("device lost ({reason:?}): {message}"),
            ),
        );
    });
}

fn surface_configuration(
    surface: &Surface<'_>,
    adapter: &wgpu::Adapter,
    initial_size: SurfaceSize,
) -> Result<(SurfaceConfiguration, TextureFormat), RenderError> {
    validate_surface_dimensions(initial_size)?;
    let capabilities = surface.get_capabilities(adapter);
    let format = choose_surface_format(&capabilities.formats)?;
    let present_mode = capabilities
        .present_modes
        .iter()
        .copied()
        .find(|mode| *mode == PresentMode::Fifo)
        .or_else(|| capabilities.present_modes.first().copied())
        .ok_or_else(|| {
            RenderError::new(
                RenderErrorKind::SurfaceCapabilities,
                "surface reports no presentation modes",
            )
        })?;
    let alpha_mode = capabilities.alpha_modes.first().copied().ok_or_else(|| {
        RenderError::new(
            RenderErrorKind::SurfaceCapabilities,
            "surface reports no alpha modes",
        )
    })?;
    Ok((
        SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: SurfaceColorSpace::Auto,
            width: initial_size.width.max(1),
            height: initial_size.height.max(1),
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        },
        format,
    ))
}

fn record_async_error(shared: &Mutex<Option<RenderError>>, error: RenderError) {
    if let Ok(mut stored) = shared.lock() {
        if stored.is_none() {
            *stored = Some(error);
        }
    }
}

fn choose_surface_format(formats: &[TextureFormat]) -> Result<TextureFormat, RenderError> {
    formats
        .iter()
        .copied()
        .find(TextureFormat::is_srgb)
        .or_else(|| formats.first().copied())
        .ok_or_else(|| {
            RenderError::new(
                RenderErrorKind::SurfaceCapabilities,
                "surface reports no supported texture formats",
            )
        })
}

fn validate_surface_dimensions(size: SurfaceSize) -> Result<(), RenderError> {
    if size.width > u32::from(u16::MAX) || size.height > u32::from(u16::MAX) {
        return Err(RenderError::new(
            RenderErrorKind::SurfaceCapabilities,
            "surface dimensions exceed the renderer's exact viewport range",
        ));
    }
    Ok(())
}

fn validate_camera(camera: Camera2D) -> Result<(), RenderError> {
    if !camera.center[0].is_finite()
        || !camera.center[1].is_finite()
        || !camera.zoom.is_finite()
        || camera.zoom <= 0.0
    {
        return Err(RenderError::new(
            RenderErrorKind::InvalidCamera,
            "camera center must be finite and zoom must be finite and positive",
        ));
    }
    Ok(())
}

fn view_projection(
    camera: Camera2D,
    width: u32,
    height: u32,
) -> Result<[[f32; 4]; 4], RenderError> {
    let width = u16::try_from(width.max(1)).map(f32::from).map_err(|_| {
        RenderError::new(
            RenderErrorKind::SurfaceCapabilities,
            "surface width exceeds the renderer's exact viewport range",
        )
    })?;
    let height = u16::try_from(height.max(1)).map(f32::from).map_err(|_| {
        RenderError::new(
            RenderErrorKind::SurfaceCapabilities,
            "surface height exceeds the renderer's exact viewport range",
        )
    })?;
    let aspect_scale = if width >= height {
        [height / width, 1.0]
    } else {
        [1.0, width / height]
    };
    let scale_x = aspect_scale[0] * camera.zoom;
    let scale_y = -aspect_scale[1] * camera.zoom;
    let matrix = [
        [scale_x, 0.0, 0.0, 0.0],
        [0.0, scale_y, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [
            -camera.center[0] * scale_x,
            -camera.center[1] * scale_y,
            0.0,
            1.0,
        ],
    ];
    if matrix.iter().flatten().all(|value| value.is_finite()) {
        Ok(matrix)
    } else {
        Err(RenderError::new(
            RenderErrorKind::InvalidCamera,
            "camera transform exceeds the finite presentation range",
        ))
    }
}

fn screen_projection(width: u32, height: u32) -> Result<[[f32; 4]; 4], RenderError> {
    let width = u16::try_from(width.max(1)).map(f32::from).map_err(|_| {
        RenderError::new(
            RenderErrorKind::SurfaceCapabilities,
            "surface width exceeds the renderer's exact viewport range",
        )
    })?;
    let height = u16::try_from(height.max(1)).map(f32::from).map_err(|_| {
        RenderError::new(
            RenderErrorKind::SurfaceCapabilities,
            "surface height exceeds the renderer's exact viewport range",
        )
    })?;
    Ok([
        [2.0 / width, 0.0, 0.0, 0.0],
        [0.0, -2.0 / height, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [-1.0, 1.0, 0.0, 1.0],
    ])
}

fn validate_projected_vertices(
    vertices: &[Vertex],
    matrix: [[f32; 4]; 4],
) -> Result<(), RenderError> {
    for vertex in vertices {
        let x =
            vertex.position[0] * matrix[0][0] + vertex.position[1] * matrix[1][0] + matrix[3][0];
        let y =
            vertex.position[0] * matrix[0][1] + vertex.position[1] * matrix[1][1] + matrix[3][1];
        if !x.is_finite() || !y.is_finite() {
            return Err(RenderError::new(
                RenderErrorKind::InvalidScene,
                "scene geometry projects outside the finite clip-coordinate range",
            ));
        }
    }
    Ok(())
}

fn matrix_bytes(matrix: [[f32; 4]; 4]) -> [u8; 64] {
    let mut bytes = [0_u8; 64];
    for (index, value) in matrix.into_iter().flatten().enumerate() {
        bytes[index * size_of::<f32>()..(index + 1) * size_of::<f32>()]
            .copy_from_slice(&value.to_ne_bytes());
    }
    bytes
}

fn matrix_pair_bytes(world: [[f32; 4]; 4], screen: [[f32; 4]; 4]) -> [u8; 128] {
    let mut bytes = [0_u8; 128];
    bytes[..64].copy_from_slice(&matrix_bytes(world));
    bytes[64..].copy_from_slice(&matrix_bytes(screen));
    bytes
}

fn create_scene_resources(
    device: &Device,
    queue: &Queue,
    surface_format: TextureFormat,
    async_error: &Arc<Mutex<Option<RenderError>>>,
) -> Result<SceneResources, RenderError> {
    let scope = device.push_error_scope(ErrorFilter::Validation);
    let shader = device.create_shader_module(ShaderModuleDescriptor {
        label: Some("hycel 2d WGSL scene shader"),
        source: ShaderSource::Wgsl(SCENE_SHADER.into()),
    });
    let camera_buffer = device.create_buffer(&BufferDescriptor {
        label: Some("hycel camera and overlay projection uniforms"),
        size: 128,
        usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let texture_bind_group_layout = create_scene_bind_group_layout(device);
    let pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
        label: Some("hycel 2d scene pipeline layout"),
        bind_group_layouts: &[Some(&texture_bind_group_layout)],
        immediate_size: 0,
    });
    let pipeline = create_scene_pipeline(device, &shader, &pipeline_layout, surface_format);
    let white = RgbaImage::new(1, 1, vec![255; 4])?;
    let checker = RgbaImage::new(2, 2, TEXTURE_PIXELS.to_vec())?;
    let textures = vec![
        create_texture_resource(
            device,
            queue,
            &texture_bind_group_layout,
            &camera_buffer,
            white,
            "hycel white texture",
        )?,
        create_texture_resource(
            device,
            queue,
            &texture_bind_group_layout,
            &camera_buffer,
            checker,
            "hycel checker demonstration texture",
        )?,
    ];
    if let Some(error) = pollster::block_on(scope.pop()) {
        let error = RenderError::new(RenderErrorKind::ShaderValidation, error.to_string());
        record_async_error(async_error, error.clone());
        return Err(error);
    }
    Ok(SceneResources {
        pipeline,
        camera_buffer,
        texture_bind_group_layout,
        textures,
    })
}

fn create_texture_resource(
    device: &Device,
    queue: &Queue,
    bind_group_layout: &BindGroupLayout,
    camera_buffer: &Buffer,
    image: RgbaImage,
    label: &str,
) -> Result<GpuTexture, RenderError> {
    if image.width() > device.limits().max_texture_dimension_2d
        || image.height() > device.limits().max_texture_dimension_2d
    {
        return Err(RenderError::new(
            RenderErrorKind::InvalidTextureData,
            "texture dimensions exceed the device limit",
        ));
    }
    let (width, height, pixels) = image.into_parts();
    let sampler = device.create_sampler(&SamplerDescriptor {
        label: Some("hycel nearest-filtered texture sampler"),
        mag_filter: FilterMode::Nearest,
        min_filter: FilterMode::Nearest,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });
    let texture = device.create_texture(&TextureDescriptor {
        label: Some(label),
        size: Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: TextureFormat::Rgba8UnormSrgb,
        usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&TextureViewDescriptor::default());
    let bind_group = device.create_bind_group(&BindGroupDescriptor {
        label: Some("hycel scene texture and projection bindings"),
        layout: bind_group_layout,
        entries: &[
            BindGroupEntry {
                binding: 0,
                resource: BindingResource::Sampler(&sampler),
            },
            BindGroupEntry {
                binding: 1,
                resource: BindingResource::TextureView(&view),
            },
            BindGroupEntry {
                binding: 2,
                resource: camera_buffer.as_entire_binding(),
            },
        ],
    });
    Ok(GpuTexture {
        _texture: texture,
        _view: view,
        _sampler: sampler,
        bind_group,
    })
}

fn create_scene_bind_group_layout(device: &Device) -> BindGroupLayout {
    device.create_bind_group_layout(&BindGroupLayoutDescriptor {
        label: Some("hycel 2d scene bindings"),
        entries: &[
            BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: BindingType::Sampler(SamplerBindingType::Filtering),
                count: None,
            },
            BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: BindingType::Texture {
                    sample_type: TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    })
}

fn create_scene_pipeline(
    device: &Device,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    surface_format: TextureFormat,
) -> RenderPipeline {
    let attributes = wgpu::vertex_attr_array![
        0 => Float32x2,
        1 => Float32x2,
        2 => Float32x4,
        3 => Float32,
    ];
    let vertex_layout = VertexBufferLayout {
        array_stride: size_of::<Vertex>() as wgpu::BufferAddress,
        step_mode: VertexStepMode::Vertex,
        attributes: &attributes,
    };
    device.create_render_pipeline(&RenderPipelineDescriptor {
        label: Some("hycel colored and textured quad pipeline"),
        layout: Some(layout),
        vertex: VertexState {
            module: shader,
            entry_point: Some("vertex_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[Some(vertex_layout)],
        },
        primitive: PrimitiveState {
            topology: PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(FragmentState {
            module: shader,
            entry_point: Some("fragment_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(ColorTargetState {
                format: surface_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn vertex_bytes(vertices: &[Vertex]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(std::mem::size_of_val(vertices));
    for vertex in vertices {
        for value in vertex
            .position
            .into_iter()
            .chain(vertex.uv)
            .chain(vertex.color)
            .chain([vertex.screen_space])
        {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
    }
    bytes
}

fn native_backend() -> Backends {
    #[cfg(target_os = "macos")]
    {
        Backends::METAL
    }
    #[cfg(target_os = "windows")]
    {
        Backends::DX12
    }
    #[cfg(target_os = "linux")]
    {
        Backends::VULKAN
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Backends::all()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Camera2D, RenderError, RenderErrorKind, choose_surface_format, matrix_bytes,
        screen_projection, validate_camera, validate_surface_dimensions, view_projection,
    };
    use hycel_platform::SurfaceSize;
    use wgpu::TextureFormat;

    #[test]
    fn camera_view_projection_centers_and_scales_without_distorting_aspect() {
        let matrix = view_projection(
            Camera2D {
                center: [0.5, -0.25],
                zoom: 2.0,
            },
            1600,
            900,
        )
        .unwrap();
        assert!((matrix[0][0] - 1.125).abs() < f32::EPSILON);
        assert!((matrix[1][1] + 2.0).abs() < f32::EPSILON);
        assert!((matrix[3][0] + 0.5625).abs() < f32::EPSILON);
        assert!((matrix[3][1] + 0.5).abs() < f32::EPSILON);
        assert_eq!(matrix_bytes(matrix).len(), 64);
    }

    #[test]
    fn screen_projection_maps_pixel_coordinates_into_top_left_origin_clip_space() {
        let matrix = screen_projection(800, 450).unwrap();
        assert!((matrix[0][0] - 2.0 / 800.0).abs() < f32::EPSILON);
        assert!((matrix[1][1] + 2.0 / 450.0).abs() < f32::EPSILON);
        assert!((matrix[3][0] + 1.0).abs() < f32::EPSILON);
        assert!((matrix[3][1] - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn camera_validation_rejects_non_finite_and_non_positive_values() {
        assert_eq!(
            validate_camera(Camera2D {
                center: [f32::NAN, 0.0],
                zoom: 1.0,
            })
            .unwrap_err()
            .kind(),
            RenderErrorKind::InvalidCamera
        );
        assert!(
            validate_camera(Camera2D {
                center: [0.0, 0.0],
                zoom: 0.0,
            })
            .is_err()
        );
    }

    #[test]
    fn surface_dimensions_reject_values_outside_the_exact_f32_range() {
        assert_eq!(
            validate_surface_dimensions(SurfaceSize {
                width: u32::MAX,
                height: 1,
            })
            .unwrap_err()
            .kind(),
            RenderErrorKind::SurfaceCapabilities
        );
        assert!(
            validate_surface_dimensions(SurfaceSize {
                width: u16::MAX.into(),
                height: u16::MAX.into(),
            })
            .is_ok()
        );
    }

    #[test]
    fn camera_rejects_viewports_outside_exact_f32_dimension_range() {
        assert_eq!(
            view_projection(Camera2D::default(), u32::MAX, 1)
                .unwrap_err()
                .kind(),
            RenderErrorKind::SurfaceCapabilities
        );
    }

    #[test]
    fn camera_view_projection_rejects_overflowed_transforms() {
        assert_eq!(
            view_projection(
                Camera2D {
                    center: [f32::MAX, 0.0],
                    zoom: f32::MAX,
                },
                1600,
                900,
            )
            .unwrap_err()
            .kind(),
            RenderErrorKind::InvalidCamera
        );
    }

    #[test]
    fn surface_format_prefers_srgb_and_rejects_empty_capabilities() {
        assert_eq!(
            choose_surface_format(&[TextureFormat::Bgra8Unorm, TextureFormat::Rgba8UnormSrgb])
                .unwrap(),
            TextureFormat::Rgba8UnormSrgb
        );
        assert_eq!(
            choose_surface_format(&[TextureFormat::Bgra8Unorm]).unwrap(),
            TextureFormat::Bgra8Unorm
        );
        assert_eq!(
            choose_surface_format(&[]).unwrap_err().kind(),
            RenderErrorKind::SurfaceCapabilities
        );
    }

    #[test]
    fn render_error_is_a_human_readable_error() {
        let error = RenderError::new(RenderErrorKind::Surface, "surface lost");
        assert!(error.to_string().contains("surface lost"));
    }
}
