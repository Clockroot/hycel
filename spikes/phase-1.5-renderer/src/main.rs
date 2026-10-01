use std::{
    error::Error,
    sync::Arc,
    time::{Duration, Instant},
};

use wgpu::{
    Backends, Color, CommandEncoderDescriptor, CurrentSurfaceTexture, Device, Instance, LoadOp,
    Operations, PresentMode, Queue, RenderPassColorAttachment, RenderPassDescriptor, StoreOp,
    Surface, SurfaceColorSpace, SurfaceConfiguration, TextureUsages, TextureViewDescriptor,
};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{Window, WindowAttributes, WindowId},
};

enum FrameProblem {
    Reconfigure,
    Validation,
}

struct Probe {
    window: Option<Arc<Window>>,
    surface: Option<Surface<'static>>,
    device: Option<Device>,
    queue: Option<Queue>,
    config: Option<SurfaceConfiguration>,
    started_at: Instant,
    presented_frames: u32,
    pressed_space: bool,
    smoke: bool,
    failure: Option<String>,
}

impl Probe {
    fn new(smoke: bool) -> Self {
        Self {
            window: None,
            surface: None,
            device: None,
            queue: None,
            config: None,
            started_at: Instant::now(),
            presented_frames: 0,
            pressed_space: false,
            smoke,
            failure: None,
        }
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, message: impl Into<String>) {
        self.failure = Some(message.into());
        event_loop.exit();
    }

    fn create_renderer(&mut self, window: Arc<Window>) -> Result<(), String> {
        let mut instance_descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        instance_descriptor.backends = native_backend();
        let instance = Instance::new(instance_descriptor);
        let surface = instance
            .create_surface(window.clone())
            .map_err(|error| format!("create window surface: {error}"))?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .map_err(|error| format!("request native graphics adapter: {error}"))?;
        let info = adapter.get_info();
        eprintln!(
            "adapter={} backend={:?} device_type={:?}",
            info.name, info.backend, info.device_type
        );
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .map_err(|error| format!("request graphics device: {error}"))?;
        let size = window.inner_size();
        let capabilities = surface.get_capabilities(&adapter);
        let format = capabilities
            .formats
            .first()
            .copied()
            .ok_or_else(|| "surface reports no supported texture formats".to_owned())?;
        let config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: SurfaceColorSpace::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: if capabilities.present_modes.contains(&PresentMode::Fifo) {
                PresentMode::Fifo
            } else {
                *capabilities
                    .present_modes
                    .first()
                    .ok_or_else(|| "surface reports no presentation modes".to_owned())?
            },
            alpha_mode: capabilities.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        self.window = Some(window);
        self.surface = Some(surface);
        self.device = Some(device);
        self.queue = Some(queue);
        self.config = Some(config);
        Ok(())
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        let (Some(surface), Some(device), Some(config)) =
            (&self.surface, &self.device, &mut self.config)
        else {
            return;
        };
        if size.width == 0 || size.height == 0 {
            return;
        }
        config.width = size.width;
        config.height = size.height;
        surface.configure(device, config);
    }

    fn handle_key(&mut self, key: PhysicalKey, state: ElementState) -> bool {
        match key {
            PhysicalKey::Code(KeyCode::Escape) => state == ElementState::Pressed,
            PhysicalKey::Code(KeyCode::Space) => {
                self.pressed_space = state == ElementState::Pressed;
                false
            }
            _ => false,
        }
    }

    fn render(&mut self) -> Result<(), FrameProblem> {
        let (Some(surface), Some(device), Some(queue)) = (&self.surface, &self.device, &self.queue)
        else {
            return Ok(());
        };
        let frame = match surface.get_current_texture() {
            CurrentSurfaceTexture::Success(frame) | CurrentSurfaceTexture::Suboptimal(frame) => {
                frame
            }
            CurrentSurfaceTexture::Timeout | CurrentSurfaceTexture::Occluded => return Ok(()),
            CurrentSurfaceTexture::Outdated | CurrentSurfaceTexture::Lost => {
                return Err(FrameProblem::Reconfigure);
            }
            CurrentSurfaceTexture::Validation => return Err(FrameProblem::Validation),
        };
        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor {
            label: Some("hycel phase 1.5 feasibility clear"),
        });
        let color = if self.pressed_space {
            Color {
                r: 0.12,
                g: 0.45,
                b: 0.85,
                a: 1.0,
            }
        } else {
            Color {
                r: 0.12,
                g: 0.65,
                b: 0.33,
                a: 1.0,
            }
        };
        {
            let _pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("hycel phase 1.5 feasibility clear pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(color),
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        queue.submit([encoder.finish()]);
        queue.present(frame);
        self.presented_frames += 1;
        Ok(())
    }
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes: WindowAttributes = Window::default_attributes()
            .with_title("Hycel Phase 1.5 Window / Input / Renderer Spike")
            .with_inner_size(PhysicalSize::new(640, 360));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                self.fail(event_loop, format!("create native window: {error}"));
                return;
            }
        };
        if let Err(error) = self.create_renderer(window) {
            self.fail(event_loop, error);
        } else {
            // Start the smoke window only after blocking adapter/device setup;
            // otherwise slow Windows software adapters can consume the entire
            // timeout before the first redraw is ever requested.
            self.started_at = Instant::now();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => self.resize(size),
            WindowEvent::Focused(false) => self.pressed_space = false,
            WindowEvent::KeyboardInput { event, .. } => {
                if self.handle_key(event.physical_key, event.state) {
                    event_loop.exit();
                }
            }
            WindowEvent::RedrawRequested => match self.render() {
                Ok(()) => {}
                Err(FrameProblem::Reconfigure) => {
                    if let Some(window) = &self.window {
                        self.resize(window.inner_size());
                    }
                }
                Err(FrameProblem::Validation) => {
                    self.fail(event_loop, "graphics surface validation failed");
                }
            },
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
        if self.smoke && self.started_at.elapsed() >= Duration::from_secs(3) {
            event_loop.exit();
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        eprintln!("presented_frames={}", self.presented_frames);
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

fn main() -> Result<(), Box<dyn Error>> {
    let smoke = std::env::args().any(|argument| argument == "--smoke");
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut probe = Probe::new(smoke);
    event_loop.run_app(&mut probe)?;
    if let Some(error) = probe.failure {
        return Err(error.into());
    }
    if smoke && probe.presented_frames == 0 {
        return Err("window closed before presenting any frames".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn space_key_switches_and_releases_the_clear_color() {
        let mut probe = Probe::new(true);
        let space = PhysicalKey::Code(KeyCode::Space);

        assert!(!probe.handle_key(space, ElementState::Pressed));
        assert!(probe.pressed_space);
        assert!(!probe.handle_key(space, ElementState::Released));
        assert!(!probe.pressed_space);
    }

    #[test]
    fn escape_closes_only_when_pressed() {
        let mut probe = Probe::new(true);
        let escape = PhysicalKey::Code(KeyCode::Escape);

        assert!(!probe.handle_key(escape, ElementState::Released));
        assert!(probe.handle_key(escape, ElementState::Pressed));
    }
}
