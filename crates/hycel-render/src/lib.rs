//! A minimal safe 2D presentation backend using `wgpu`.
//!
//! The crate owns native graphics objects and exposes only Hycel types. It draws a camera-aware
//! colored quad and a sampled texture to a [`hycel_platform::WindowHandle`] surface. This is an
//! initial rendering slice, not yet a complete sprite/resource pipeline.

use std::{
    error::Error,
    fmt,
    mem::size_of,
    sync::{Arc, Mutex},
};

use hycel_platform::{SurfaceSize, WindowHandle};
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
const VERTICES: [Vertex; 12] = scene_vertices();
const VERTEX_COUNT: u32 = 12;
const TEXTURE_PIXELS: [u8; 16] = [
    255, 72, 72, 255, 64, 128, 255, 255, 255, 220, 48, 255, 64, 220, 128, 255,
];

/// A simple camera in presentation world units. Positive zoom magnifies the scene.
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
    vertex_buffer: Buffer,
    view_buffer: Buffer,
    bind_group: BindGroup,
    bind_group_layout: BindGroupLayout,
    texture: Texture,
    texture_view: TextureView,
    sampler: Sampler,
}

/// Owns the GPU surface, device, scene pipeline, and a small demonstration texture.
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
    vertex_buffer: Buffer,
    view_buffer: Buffer,
    bind_group: BindGroup,
    _bind_group_layout: BindGroupLayout,
    _texture: Texture,
    _texture_view: TextureView,
    _sampler: Sampler,
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
            vertex_buffer: resources.vertex_buffer,
            view_buffer: resources.view_buffer,
            bind_group: resources.bind_group,
            _bind_group_layout: resources.bind_group_layout,
            _texture: resources.texture,
            _texture_view: resources.texture_view,
            _sampler: resources.sampler,
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
        validate_camera(camera)?;
        self.check_async_error()?;
        if self.suspended {
            return Ok(FrameOutcome::Skipped);
        }

        let matrix = view_projection(camera, self.config.width, self.config.height)?;
        self.queue
            .write_buffer(&self.view_buffer, 0, &matrix_bytes(matrix));
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
        self.draw_frame(frame);
        self.check_async_error()?;
        Ok(FrameOutcome::Presented)
    }

    fn draw_frame(&mut self, frame: SurfaceTexture) {
        let view = frame.texture.create_view(&TextureViewDescriptor::default());
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
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            pass.draw(0..VERTEX_COUNT, 0..1);
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
    let scale_y = aspect_scale[1] * camera.zoom;
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

fn matrix_bytes(matrix: [[f32; 4]; 4]) -> [u8; 64] {
    let mut bytes = [0_u8; 64];
    for (index, value) in matrix.into_iter().flatten().enumerate() {
        bytes[index * size_of::<f32>()..(index + 1) * size_of::<f32>()]
            .copy_from_slice(&value.to_ne_bytes());
    }
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
    let texture = create_demo_texture(device, queue);
    let view_buffer = device.create_buffer(&BufferDescriptor {
        label: Some("hycel camera view-projection uniform"),
        size: 64,
        usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind_group_layout = create_scene_bind_group_layout(device);
    let bind_group = device.create_bind_group(&BindGroupDescriptor {
        label: Some("hycel 2d scene bind group"),
        layout: &bind_group_layout,
        entries: &[
            BindGroupEntry {
                binding: 0,
                resource: BindingResource::Sampler(&texture.sampler),
            },
            BindGroupEntry {
                binding: 1,
                resource: BindingResource::TextureView(&texture.view),
            },
            BindGroupEntry {
                binding: 2,
                resource: view_buffer.as_entire_binding(),
            },
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
        label: Some("hycel 2d scene pipeline layout"),
        bind_group_layouts: &[Some(&bind_group_layout)],
        immediate_size: 0,
    });
    let pipeline = create_scene_pipeline(device, &shader, &pipeline_layout, surface_format);
    let vertex_data = vertex_bytes(&VERTICES);
    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("hycel scene quad vertices"),
        contents: &vertex_data,
        usage: BufferUsages::VERTEX,
    });
    if let Some(error) = pollster::block_on(scope.pop()) {
        let error = RenderError::new(RenderErrorKind::ShaderValidation, error.to_string());
        record_async_error(async_error, error.clone());
        return Err(error);
    }
    Ok(SceneResources {
        pipeline,
        vertex_buffer,
        view_buffer,
        bind_group,
        bind_group_layout,
        texture: texture.texture,
        texture_view: texture.view,
        sampler: texture.sampler,
    })
}

struct DemoTexture {
    texture: Texture,
    view: TextureView,
    sampler: Sampler,
}

fn create_demo_texture(device: &Device, queue: &Queue) -> DemoTexture {
    let sampler = device.create_sampler(&SamplerDescriptor {
        label: Some("hycel scene texture sampler"),
        mag_filter: FilterMode::Nearest,
        min_filter: FilterMode::Nearest,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });
    let texture = device.create_texture(&TextureDescriptor {
        label: Some("hycel checker demonstration texture"),
        size: Extent3d {
            width: 2,
            height: 2,
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
        &TEXTURE_PIXELS,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(8),
            rows_per_image: Some(2),
        },
        Extent3d {
            width: 2,
            height: 2,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&TextureViewDescriptor::default());
    DemoTexture {
        texture,
        view,
        sampler,
    }
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
            .chain([vertex.textured])
        {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
    }
    bytes
}

#[derive(Clone, Copy)]
struct Vertex {
    position: [f32; 2],
    uv: [f32; 2],
    color: [f32; 4],
    textured: f32,
}

const fn scene_vertices() -> [Vertex; 12] {
    let solid = [1.0, 0.48, 0.12, 1.0];
    let white = [1.0, 1.0, 1.0, 1.0];
    [
        vertex([-0.82, -0.55], [0.0, 1.0], solid, 0.0),
        vertex([-0.22, -0.55], [1.0, 1.0], solid, 0.0),
        vertex([-0.22, 0.55], [1.0, 0.0], solid, 0.0),
        vertex([-0.82, -0.55], [0.0, 1.0], solid, 0.0),
        vertex([-0.22, 0.55], [1.0, 0.0], solid, 0.0),
        vertex([-0.82, 0.55], [0.0, 0.0], solid, 0.0),
        vertex([0.22, -0.55], [0.0, 1.0], white, 1.0),
        vertex([0.82, -0.55], [1.0, 1.0], white, 1.0),
        vertex([0.82, 0.55], [1.0, 0.0], white, 1.0),
        vertex([0.22, -0.55], [0.0, 1.0], white, 1.0),
        vertex([0.82, 0.55], [1.0, 0.0], white, 1.0),
        vertex([0.22, 0.55], [0.0, 0.0], white, 1.0),
    ]
}

const fn vertex(position: [f32; 2], uv: [f32; 2], color: [f32; 4], textured: f32) -> Vertex {
    Vertex {
        position,
        uv,
        color,
        textured,
    }
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
        validate_camera, validate_surface_dimensions, view_projection,
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
        assert!((matrix[1][1] - 2.0).abs() < f32::EPSILON);
        assert!((matrix[3][0] + 0.5625).abs() < f32::EPSILON);
        assert!((matrix[3][1] - 0.5).abs() < f32::EPSILON);
        assert_eq!(matrix_bytes(matrix).len(), 64);
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
