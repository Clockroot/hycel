use std::{
    cell::Cell,
    error::Error,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use hycel_platform::{
    EventAction, PlatformEvent, SurfaceSize, WindowConfig, WindowHandle, run_window,
};

fn main() -> Result<(), Box<dyn Error>> {
    let finished = Arc::new(AtomicBool::new(false));
    let watchdog_finished = finished.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(20));
        if !watchdog_finished.load(Ordering::Acquire) {
            eprintln!("window lifecycle smoke timed out after 20 seconds");
            std::process::exit(1);
        }
    });
    let result = run_smoke();
    finished.store(true, Ordering::Release);
    result
}

fn run_smoke() -> Result<(), Box<dyn Error>> {
    let mut window: Option<WindowHandle> = None;
    let frames = Rc::new(Cell::new(0_u32));
    let smoke_started_at = Rc::new(Cell::new(None));
    let frame_count = frames.clone();
    let smoke_started_at_for_callback = smoke_started_at.clone();
    run_window(
        WindowConfig::new("Hycel Phase 4.2 window lifecycle smoke", 640, 360)?,
        move |event| match event {
            PlatformEvent::WindowCreated {
                window: created,
                metrics,
            } => {
                eprintln!(
                    "window_created={}x{} scale_factor={}",
                    metrics.surface_size.width, metrics.surface_size.height, metrics.scale_factor
                );
                window = Some(created);
                smoke_started_at_for_callback.set(Some(Instant::now()));
                window.as_ref().map(WindowHandle::request_redraw);
                EventAction::Continue
            }
            PlatformEvent::Resized(SurfaceSize { width, height }) => {
                eprintln!("resized={width}x{height}");
                EventAction::Continue
            }
            PlatformEvent::ScaleFactorChanged(scale_factor) => {
                eprintln!("scale_factor_changed={scale_factor}");
                EventAction::Continue
            }
            PlatformEvent::FocusChanged(focused) => {
                eprintln!("focused={focused}");
                EventAction::Continue
            }
            PlatformEvent::RedrawRequested => {
                let frames = frame_count.get() + 1;
                frame_count.set(frames);
                if frames == 1 {
                    eprintln!("redraw_frames={frames}");
                }
                if frames >= 30
                    || smoke_started_at_for_callback
                        .get()
                        .is_some_and(|started| started.elapsed().as_secs() >= 5)
                {
                    EventAction::Exit
                } else {
                    window.as_ref().map(WindowHandle::request_redraw);
                    EventAction::Continue
                }
            }
            PlatformEvent::CloseRequested => EventAction::Exit,
        },
    )?;
    if frames.get() == 0 {
        return Err("window event loop exited before the first redraw".into());
    }
    eprintln!("window_lifecycle_smoke_frames={}", frames.get());
    Ok(())
}
