use std::{
    cell::Cell,
    error::Error,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use hycel_platform::{EventAction, PlatformEvent, WindowConfig, WindowHandle, run_window};
use hycel_render::{Camera2D, FrameOutcome, RenderError, Renderer};

fn main() -> Result<(), Box<dyn Error>> {
    let finished = Arc::new(AtomicBool::new(false));
    let watchdog_finished = finished.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(30));
        if !watchdog_finished.load(Ordering::Acquire) {
            eprintln!("Hycel render smoke timed out after 30 seconds");
            std::process::exit(1);
        }
    });
    let result = run_smoke();
    finished.store(true, Ordering::Release);
    result
}

fn run_smoke() -> Result<(), Box<dyn Error>> {
    let mut window: Option<WindowHandle> = None;
    let mut renderer: Option<Renderer> = None;
    let frames = Rc::new(Cell::new(0_u32));
    let frame_count = frames.clone();
    let error = Rc::new(Cell::new(None::<RenderError>));
    let render_error = error.clone();

    run_window(
        WindowConfig::new("Hycel Phase 4.3 render smoke", 800, 450)?,
        move |event| match event {
            PlatformEvent::WindowCreated {
                window: created,
                metrics,
            } => {
                eprintln!(
                    "render_window={}x{} scale_factor={}",
                    metrics.surface_size.width, metrics.surface_size.height, metrics.scale_factor
                );
                match Renderer::new(Arc::new(created.clone())) {
                    Ok(created_renderer) => {
                        renderer = Some(created_renderer);
                        window = Some(created);
                        window.as_ref().map(WindowHandle::request_redraw);
                        EventAction::Continue
                    }
                    Err(failure) => {
                        render_error.set(Some(failure));
                        EventAction::Exit
                    }
                }
            }
            PlatformEvent::Resized(size) => {
                if let Some(renderer) = &mut renderer {
                    if let Err(failure) = renderer.resize(size) {
                        render_error.set(Some(failure));
                        return EventAction::Exit;
                    }
                }
                window.as_ref().map(WindowHandle::request_redraw);
                EventAction::Continue
            }
            PlatformEvent::ScaleFactorChanged(_) | PlatformEvent::FocusChanged(_) => {
                EventAction::Continue
            }
            PlatformEvent::RedrawRequested => {
                let Some(renderer) = &mut renderer else {
                    return EventAction::Exit;
                };
                match renderer.render(Camera2D::default()) {
                    Ok(FrameOutcome::Presented) => {
                        let next_count = frame_count.get() + 1;
                        frame_count.set(next_count);
                        if next_count == 1 || next_count == 30 {
                            eprintln!("presented_frames={next_count}");
                        }
                        if next_count >= 30 {
                            EventAction::Exit
                        } else {
                            window.as_ref().map(WindowHandle::request_redraw);
                            EventAction::Continue
                        }
                    }
                    Ok(FrameOutcome::Skipped | FrameOutcome::Reconfigured) => {
                        window.as_ref().map(WindowHandle::request_redraw);
                        EventAction::Continue
                    }
                    Err(failure) => {
                        render_error.set(Some(failure));
                        EventAction::Exit
                    }
                }
            }
            PlatformEvent::CloseRequested => EventAction::Exit,
        },
    )?;

    if let Some(failure) = error.take() {
        return Err(failure.into());
    }
    if frames.get() != 30 {
        return Err(format!("expected 30 presented frames, got {}", frames.get()).into());
    }
    Ok(())
}
