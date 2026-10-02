//! Safe desktop-window lifecycle and event abstractions for Hycel.
//!
//! Native `winit` types are kept private to this crate. A [`WindowHandle`] is an opaque,
//! cloneable handle that also implements `raw-window-handle` traits for internal renderer
//! integration; game-facing code should use its Hycel-owned methods and event types.

use std::{cell::RefCell, error::Error, fmt, rc::Rc, sync::Arc};

use hycel_input::{InputEvent, KeyCode, MouseButton};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalSize},
    error::EventLoopError,
    event::{ElementState, WindowEvent as WinitWindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode as NativeKeyCode, PhysicalKey},
    raw_window_handle::{HasDisplayHandle, HasWindowHandle},
    window::{Window as NativeWindow, WindowAttributes, WindowId},
};

/// Initial logical client-area dimensions. These are presentation pixels, not simulation units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogicalWindowSize {
    /// Requested logical width.
    pub width: u32,
    /// Requested logical height.
    pub height: u32,
}

impl LogicalWindowSize {
    /// Creates a non-zero logical size.
    ///
    /// # Errors
    ///
    /// Returns [`WindowConfigError::ZeroSize`] if either dimension is zero.
    pub fn new(width: u32, height: u32) -> Result<Self, WindowConfigError> {
        if width == 0 || height == 0 {
            return Err(WindowConfigError::ZeroSize);
        }
        Ok(Self { width, height })
    }
}

/// Physical surface dimensions in pixels. A zero dimension is possible while minimized.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SurfaceSize {
    /// Width in physical pixels.
    pub width: u32,
    /// Height in physical pixels.
    pub height: u32,
}

impl From<PhysicalSize<u32>> for SurfaceSize {
    fn from(size: PhysicalSize<u32>) -> Self {
        Self {
            width: size.width,
            height: size.height,
        }
    }
}

/// Current presentation dimensions and OS scale factor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowMetrics {
    /// Current physical client-area size.
    pub surface_size: SurfaceSize,
    /// OS-reported logical-to-physical scale factor.
    pub scale_factor: f64,
}

/// Options for creating the initial desktop window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowConfig {
    title: String,
    logical_size: LogicalWindowSize,
    resizable: bool,
}

impl WindowConfig {
    /// Creates a resizable window configuration.
    ///
    /// # Errors
    ///
    /// Returns [`WindowConfigError::ZeroSize`] if either initial dimension is zero.
    pub fn new(
        title: impl Into<String>,
        width: u32,
        height: u32,
    ) -> Result<Self, WindowConfigError> {
        Ok(Self {
            title: title.into(),
            logical_size: LogicalWindowSize::new(width, height)?,
            resizable: true,
        })
    }

    /// Sets whether the user may resize the window.
    #[must_use]
    pub fn with_resizable(mut self, resizable: bool) -> Self {
        self.resizable = resizable;
        self
    }

    /// Returns the title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Returns the initial logical dimensions.
    #[must_use]
    pub const fn logical_size(&self) -> LogicalWindowSize {
        self.logical_size
    }

    /// Returns whether the window is resizable.
    #[must_use]
    pub const fn is_resizable(&self) -> bool {
        self.resizable
    }
}

/// A window event normalized into a backend-independent Hycel value.
#[derive(Clone, Debug)]
pub enum PlatformEvent {
    /// The native window is available. Keep this handle alive while a renderer surface uses it.
    WindowCreated {
        /// Opaque, cloneable native-window lifetime token.
        window: WindowHandle,
        /// Initial size and scale factor.
        metrics: WindowMetrics,
    },
    /// The physical surface dimensions changed. Zero dimensions indicate minimization.
    Resized(SurfaceSize),
    /// The OS scale factor changed. The default OS-suggested resize is used; this event does not
    /// expose a platform size-writer customization point.
    ScaleFactorChanged(f64),
    /// The window gained or lost keyboard focus.
    FocusChanged(bool),
    /// The OS requests that the application close.
    CloseRequested,
    /// The window needs its next presentation frame.
    RedrawRequested,
    /// A supported physical keyboard key or mouse button changed state.
    Input(InputEvent),
}

/// Whether the application wants the event loop to continue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventAction {
    /// Continue processing native events.
    Continue,
    /// End the event loop.
    Exit,
}

/// A cloneable window token that hides the native windowing type.
#[derive(Clone)]
pub struct WindowHandle {
    inner: Arc<dyn NativeWindow>,
}

impl WindowHandle {
    /// Returns current physical surface size in pixels.
    #[must_use]
    pub fn surface_size(&self) -> SurfaceSize {
        self.inner.surface_size().into()
    }

    /// Returns the current OS scale factor.
    #[must_use]
    pub fn scale_factor(&self) -> f64 {
        self.inner.scale_factor()
    }

    /// Requests a redraw event from the host window system.
    pub fn request_redraw(&self) {
        self.inner.request_redraw();
    }
}

impl fmt::Debug for WindowHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WindowHandle")
            .field("surface_size", &self.surface_size())
            .field("scale_factor", &self.scale_factor())
            .finish_non_exhaustive()
    }
}

impl HasWindowHandle for WindowHandle {
    fn window_handle(
        &self,
    ) -> Result<winit::raw_window_handle::WindowHandle<'_>, winit::raw_window_handle::HandleError>
    {
        self.inner.window_handle()
    }
}

impl HasDisplayHandle for WindowHandle {
    fn display_handle(
        &self,
    ) -> Result<winit::raw_window_handle::DisplayHandle<'_>, winit::raw_window_handle::HandleError>
    {
        self.inner.display_handle()
    }
}

/// Stable failure category for window/event-loop operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlatformErrorKind {
    /// The OS rejected event-loop initialization or execution.
    EventLoop,
    /// The OS rejected native window creation.
    WindowCreation,
}

/// Error creating or running a desktop window, without exposing native backend error types.
#[derive(Debug)]
pub struct PlatformError {
    kind: PlatformErrorKind,
    message: String,
    source: Option<Box<dyn Error>>,
}

impl PlatformError {
    fn event_loop(error: EventLoopError) -> Self {
        Self {
            kind: PlatformErrorKind::EventLoop,
            message: error.to_string(),
            source: Some(Box::new(error)),
        }
    }

    fn window_creation(error: impl Error + 'static) -> Self {
        Self {
            kind: PlatformErrorKind::WindowCreation,
            message: error.to_string(),
            source: Some(Box::new(error)),
        }
    }

    /// Returns the stable category for this failure.
    #[must_use]
    pub const fn kind(&self) -> PlatformErrorKind {
        self.kind
    }
}

impl fmt::Display for PlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            PlatformErrorKind::EventLoop => {
                write!(
                    f,
                    "initialize or run the desktop event loop: {}",
                    self.message
                )
            }
            PlatformErrorKind::WindowCreation => {
                write!(f, "create the desktop window: {}", self.message)
            }
        }
    }
}

impl Error for PlatformError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_deref()
    }
}

/// Invalid window configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowConfigError {
    /// Both initial logical dimensions must be non-zero.
    ZeroSize,
}

impl fmt::Display for WindowConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroSize => {
                f.write_str("initial logical window width and height must be non-zero")
            }
        }
    }
}

impl Error for WindowConfigError {}

/// Runs one native desktop window until its close request or an application exit action.
///
/// The event loop is owned by the calling thread and can only be created/run once. The callback
/// must be `'static` because the native event loop owns its handler. Call [`WindowHandle::request_redraw`]
/// from the `WindowCreated` callback or later to request presentation work.
///
/// # Errors
///
/// Returns [`PlatformError`] if the OS cannot initialize/run the event loop or create the
/// requested native window.
pub fn run_window(
    config: WindowConfig,
    callback: impl FnMut(PlatformEvent) -> EventAction + 'static,
) -> Result<(), PlatformError> {
    let event_loop = EventLoop::new().map_err(PlatformError::event_loop)?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let failure = Rc::new(RefCell::new(None));
    let app = WindowApplication {
        config,
        callback: Box::new(callback),
        window: None,
        failure: failure.clone(),
    };
    event_loop.run_app(app).map_err(PlatformError::event_loop)?;
    if let Some(error) = failure.borrow_mut().take() {
        return Err(error);
    }
    Ok(())
}

type EventCallback = Box<dyn FnMut(PlatformEvent) -> EventAction>;

struct WindowApplication {
    config: WindowConfig,
    callback: EventCallback,
    window: Option<WindowHandle>,
    failure: Rc<RefCell<Option<PlatformError>>>,
}

impl WindowApplication {
    fn emit(&mut self, event_loop: &dyn ActiveEventLoop, event: &PlatformEvent) -> bool {
        let action = (self.callback)(event.clone());
        if event_requires_exit(event, action) {
            event_loop.exit();
            true
        } else {
            false
        }
    }
}

impl ApplicationHandler for WindowApplication {
    fn can_create_surfaces(&mut self, event_loop: &dyn ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let logical_size = self.config.logical_size;
        let attributes = WindowAttributes::default()
            .with_title(self.config.title.clone())
            .with_surface_size(LogicalSize::new(
                f64::from(logical_size.width),
                f64::from(logical_size.height),
            ))
            .with_resizable(self.config.resizable);
        let window = match event_loop.create_window(attributes) {
            Ok(window) => WindowHandle {
                inner: Arc::from(window),
            },
            Err(error) => {
                *self.failure.borrow_mut() = Some(PlatformError::window_creation(error));
                event_loop.exit();
                return;
            }
        };
        let metrics = WindowMetrics {
            surface_size: window.surface_size(),
            scale_factor: window.scale_factor(),
        };
        self.window = Some(window.clone());
        self.emit(
            event_loop,
            &PlatformEvent::WindowCreated { window, metrics },
        );
    }

    fn window_event(
        &mut self,
        event_loop: &dyn ActiveEventLoop,
        _window_id: WindowId,
        event: WinitWindowEvent,
    ) {
        if let Some(event) = normalize_window_event(&event) {
            self.emit(event_loop, &event);
        }
    }
}

fn event_requires_exit(event: &PlatformEvent, action: EventAction) -> bool {
    action == EventAction::Exit || matches!(event, PlatformEvent::CloseRequested)
}

fn normalize_window_event(event: &WinitWindowEvent) -> Option<PlatformEvent> {
    match event {
        WinitWindowEvent::KeyboardInput {
            event,
            is_synthetic,
            ..
        } => {
            let PhysicalKey::Code(code) = event.physical_key else {
                return None;
            };
            Some(PlatformEvent::Input(InputEvent::Key {
                code: map_key_code(code)?,
                pressed: event.state == ElementState::Pressed,
                synthetic: *is_synthetic,
            }))
        }
        WinitWindowEvent::PointerButton {
            button: winit::event::ButtonSource::Mouse(button),
            state,
            ..
        } => Some(PlatformEvent::Input(InputEvent::MouseButton {
            button: map_mouse_button(*button)?,
            pressed: *state == ElementState::Pressed,
        })),
        WinitWindowEvent::SurfaceResized(size) => Some(PlatformEvent::Resized((*size).into())),
        WinitWindowEvent::ScaleFactorChanged { scale_factor, .. } => {
            Some(PlatformEvent::ScaleFactorChanged(*scale_factor))
        }
        WinitWindowEvent::Focused(focused) => Some(PlatformEvent::FocusChanged(*focused)),
        WinitWindowEvent::CloseRequested => Some(PlatformEvent::CloseRequested),
        WinitWindowEvent::RedrawRequested => Some(PlatformEvent::RedrawRequested),
        _ => None,
    }
}

fn map_key_code(code: NativeKeyCode) -> Option<KeyCode> {
    Some(match code {
        NativeKeyCode::KeyA => KeyCode::KeyA,
        NativeKeyCode::KeyB => KeyCode::KeyB,
        NativeKeyCode::KeyC => KeyCode::KeyC,
        NativeKeyCode::KeyD => KeyCode::KeyD,
        NativeKeyCode::KeyE => KeyCode::KeyE,
        NativeKeyCode::KeyF => KeyCode::KeyF,
        NativeKeyCode::KeyG => KeyCode::KeyG,
        NativeKeyCode::KeyH => KeyCode::KeyH,
        NativeKeyCode::KeyI => KeyCode::KeyI,
        NativeKeyCode::KeyJ => KeyCode::KeyJ,
        NativeKeyCode::KeyK => KeyCode::KeyK,
        NativeKeyCode::KeyL => KeyCode::KeyL,
        NativeKeyCode::KeyM => KeyCode::KeyM,
        NativeKeyCode::KeyN => KeyCode::KeyN,
        NativeKeyCode::KeyO => KeyCode::KeyO,
        NativeKeyCode::KeyP => KeyCode::KeyP,
        NativeKeyCode::KeyQ => KeyCode::KeyQ,
        NativeKeyCode::KeyR => KeyCode::KeyR,
        NativeKeyCode::KeyS => KeyCode::KeyS,
        NativeKeyCode::KeyT => KeyCode::KeyT,
        NativeKeyCode::KeyU => KeyCode::KeyU,
        NativeKeyCode::KeyV => KeyCode::KeyV,
        NativeKeyCode::KeyW => KeyCode::KeyW,
        NativeKeyCode::KeyX => KeyCode::KeyX,
        NativeKeyCode::KeyY => KeyCode::KeyY,
        NativeKeyCode::KeyZ => KeyCode::KeyZ,
        NativeKeyCode::Digit0 => KeyCode::Digit0,
        NativeKeyCode::Digit1 => KeyCode::Digit1,
        NativeKeyCode::Digit2 => KeyCode::Digit2,
        NativeKeyCode::Digit3 => KeyCode::Digit3,
        NativeKeyCode::Digit4 => KeyCode::Digit4,
        NativeKeyCode::Digit5 => KeyCode::Digit5,
        NativeKeyCode::Digit6 => KeyCode::Digit6,
        NativeKeyCode::Digit7 => KeyCode::Digit7,
        NativeKeyCode::Digit8 => KeyCode::Digit8,
        NativeKeyCode::Digit9 => KeyCode::Digit9,
        NativeKeyCode::ArrowLeft => KeyCode::ArrowLeft,
        NativeKeyCode::ArrowRight => KeyCode::ArrowRight,
        NativeKeyCode::ArrowUp => KeyCode::ArrowUp,
        NativeKeyCode::ArrowDown => KeyCode::ArrowDown,
        NativeKeyCode::Space => KeyCode::Space,
        NativeKeyCode::Enter => KeyCode::Enter,
        NativeKeyCode::Escape => KeyCode::Escape,
        NativeKeyCode::Tab => KeyCode::Tab,
        NativeKeyCode::Backspace => KeyCode::Backspace,
        NativeKeyCode::ShiftLeft => KeyCode::LeftShift,
        NativeKeyCode::ShiftRight => KeyCode::RightShift,
        NativeKeyCode::ControlLeft => KeyCode::LeftControl,
        NativeKeyCode::ControlRight => KeyCode::RightControl,
        NativeKeyCode::AltLeft => KeyCode::LeftAlt,
        NativeKeyCode::AltRight => KeyCode::RightAlt,
        NativeKeyCode::F1 => KeyCode::F1,
        NativeKeyCode::F2 => KeyCode::F2,
        NativeKeyCode::F3 => KeyCode::F3,
        NativeKeyCode::F4 => KeyCode::F4,
        NativeKeyCode::F5 => KeyCode::F5,
        NativeKeyCode::F6 => KeyCode::F6,
        NativeKeyCode::F7 => KeyCode::F7,
        NativeKeyCode::F8 => KeyCode::F8,
        NativeKeyCode::F9 => KeyCode::F9,
        NativeKeyCode::F10 => KeyCode::F10,
        NativeKeyCode::F11 => KeyCode::F11,
        NativeKeyCode::F12 => KeyCode::F12,
        _ => return None,
    })
}

fn map_mouse_button(button: winit::event::MouseButton) -> Option<MouseButton> {
    Some(match button {
        winit::event::MouseButton::Left => MouseButton::Left,
        winit::event::MouseButton::Right => MouseButton::Right,
        winit::event::MouseButton::Middle => MouseButton::Middle,
        winit::event::MouseButton::Back => MouseButton::Back,
        winit::event::MouseButton::Forward => MouseButton::Forward,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        LogicalWindowSize, PlatformError, PlatformErrorKind, PlatformEvent, SurfaceSize,
        WindowConfig, WindowConfigError, event_requires_exit, map_key_code, map_mouse_button,
        normalize_window_event,
    };
    use std::{error::Error, fmt};
    use winit::{
        dpi::PhysicalSize,
        event::WindowEvent as WinitWindowEvent,
        raw_window_handle::{HasDisplayHandle, HasWindowHandle},
    };

    #[test]
    fn native_physical_keys_and_mouse_buttons_map_to_stable_hycel_controls() {
        assert_eq!(
            map_key_code(winit::keyboard::KeyCode::KeyA),
            Some(hycel_input::KeyCode::KeyA)
        );
        assert_eq!(
            map_key_code(winit::keyboard::KeyCode::ArrowLeft),
            Some(hycel_input::KeyCode::ArrowLeft)
        );
        assert_eq!(map_key_code(winit::keyboard::KeyCode::CapsLock), None);
        assert_eq!(
            map_mouse_button(winit::event::MouseButton::Back),
            Some(hycel_input::MouseButton::Back)
        );
        assert_eq!(map_mouse_button(winit::event::MouseButton::Button6), None);
    }

    #[test]
    fn config_preserves_title_logical_size_and_resizability() {
        let config = WindowConfig::new("Hycel Game", 960, 540)
            .unwrap()
            .with_resizable(false);

        assert_eq!(config.title(), "Hycel Game");
        assert_eq!(
            config.logical_size(),
            LogicalWindowSize {
                width: 960,
                height: 540
            }
        );
        assert!(!config.is_resizable());
    }

    #[test]
    fn config_rejects_zero_dimensions() {
        assert_eq!(
            WindowConfig::new("Invalid", 0, 540),
            Err(WindowConfigError::ZeroSize)
        );
        assert_eq!(
            WindowConfig::new("Invalid", 960, 0),
            Err(WindowConfigError::ZeroSize)
        );
    }

    #[test]
    fn resize_events_preserve_physical_dimensions_including_zero_minimized_size() {
        assert!(matches!(
            normalize_window_event(&WinitWindowEvent::SurfaceResized(PhysicalSize::new(
                800, 600
            ))),
            Some(PlatformEvent::Resized(SurfaceSize {
                width: 800,
                height: 600
            }))
        ));
        assert!(matches!(
            normalize_window_event(&WinitWindowEvent::SurfaceResized(PhysicalSize::new(0, 600))),
            Some(PlatformEvent::Resized(SurfaceSize {
                width: 0,
                height: 600
            }))
        ));
    }

    #[test]
    fn explicit_exit_and_close_requests_end_the_event_loop() {
        assert!(event_requires_exit(
            &PlatformEvent::RedrawRequested,
            super::EventAction::Exit
        ));
        assert!(!event_requires_exit(
            &PlatformEvent::RedrawRequested,
            super::EventAction::Continue
        ));
        assert!(event_requires_exit(
            &PlatformEvent::CloseRequested,
            super::EventAction::Continue
        ));
    }

    #[test]
    fn opaque_window_handle_implements_surface_handle_traits() {
        fn assert_surface_traits<T: HasWindowHandle + HasDisplayHandle>() {}
        assert_surface_traits::<super::WindowHandle>();
    }

    #[test]
    fn window_creation_error_keeps_stable_kind_and_cause() {
        #[derive(Debug)]
        struct SyntheticOsError;

        impl fmt::Display for SyntheticOsError {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("synthetic OS failure")
            }
        }

        impl Error for SyntheticOsError {}

        let error = PlatformError::window_creation(SyntheticOsError);
        assert_eq!(error.kind(), PlatformErrorKind::WindowCreation);
        assert!(error.to_string().contains("synthetic OS failure"));
        assert_eq!(
            error.source().map(ToString::to_string).as_deref(),
            Some("synthetic OS failure")
        );
    }

    #[test]
    fn focus_close_and_redraw_events_are_normalized() {
        assert!(matches!(
            normalize_window_event(&WinitWindowEvent::Focused(false)),
            Some(PlatformEvent::FocusChanged(false))
        ));
        assert!(matches!(
            normalize_window_event(&WinitWindowEvent::CloseRequested),
            Some(PlatformEvent::CloseRequested)
        ));
        assert!(matches!(
            normalize_window_event(&WinitWindowEvent::RedrawRequested),
            Some(PlatformEvent::RedrawRequested)
        ));
    }
}
