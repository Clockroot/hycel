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

use hycel_platform::{EventAction, PlatformEvent, WindowConfig, WindowHandle, run_window};
use hycel_render::{
    Camera2D, DebugText, FrameOutcome, RenderError, Renderer, RgbaImage, Sprite, TextureId,
};

const DRAW_BENCHMARK_WARMUPS: usize = 3;
const DRAW_BENCHMARK_SAMPLES: usize = 20;

fn main() -> Result<(), Box<dyn Error>> {
    let finished = Arc::new(AtomicBool::new(false));
    let watchdog_finished = finished.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(120));
        if !watchdog_finished.load(Ordering::Acquire) {
            eprintln!("Hycel render smoke timed out after 120 seconds");
            std::process::exit(1);
        }
    });
    let result = run_smoke();
    finished.store(true, Ordering::Release);
    result
}

fn create_smoke_renderer(window: Arc<WindowHandle>) -> Result<(Renderer, TextureId), RenderError> {
    let mut renderer = Renderer::new(window)?;
    let pixels = [
        255, 96, 72, 255, 64, 160, 255, 255, 255, 208, 64, 255, 64, 232, 144, 255,
    ];
    let image = RgbaImage::new(2, 2, pixels.to_vec())?;
    let texture = renderer.load_texture(image)?;
    Ok((renderer, texture))
}

fn render_smoke_frame(
    renderer: &mut Renderer,
    texture: TextureId,
) -> Result<FrameOutcome, RenderError> {
    let sprites = [
        Sprite {
            layer: 1,
            order: 0,
            tint: [1.0, 0.48, 0.12, 1.0],
            ..Sprite::new(TextureId::WHITE, [-0.5, 0.0], [0.7, 1.0])
        },
        Sprite {
            layer: 2,
            order: 0,
            tint: [0.8, 0.9, 1.0, 0.85],
            ..Sprite::new(texture, [0.5, 0.0], [0.7, 1.0])
        },
    ];
    let overlay = [DebugText {
        position: [12, 12],
        text: "HYCEL 2D SMOKE".to_owned(),
        scale: 2,
        color: [1.0, 1.0, 1.0, 1.0],
    }];
    renderer.render_scene(Camera2D::default(), &sprites, &overlay)
}

fn benchmark_draw_workloads(
    renderer: &mut Renderer,
    texture: TextureId,
) -> Result<(), RenderError> {
    for sprite_count in [100, 1_000, 5_000] {
        let sprites = (0_usize..sprite_count)
            .map(|index| {
                let x = f32::from(u16::try_from(index % 80).expect("remainder is below 80")) - 40.0;
                let y = f32::from(u16::try_from((index / 80) % 45).expect("remainder is below 45"))
                    - 22.0;
                Sprite {
                    layer: i32::try_from(index % 4).expect("remainder is below four"),
                    order: i32::try_from(index).expect("benchmark sprite count fits i32"),
                    ..Sprite::new(texture, [x, y], [0.2, 0.2])
                }
            })
            .collect::<Vec<_>>();
        for _ in 0..DRAW_BENCHMARK_WARMUPS {
            let _ = renderer.render_scene(Camera2D::default(), &sprites, &[])?;
        }
        let mut samples = Vec::with_capacity(DRAW_BENCHMARK_SAMPLES);
        for _ in 0..DRAW_BENCHMARK_SAMPLES {
            let start = Instant::now();
            let outcome = renderer.render_scene(Camera2D::default(), &sprites, &[])?;
            let elapsed = start.elapsed();
            if outcome == FrameOutcome::Presented {
                samples.push(elapsed);
            }
        }
        if samples.is_empty() {
            eprintln!(
                "draw_baseline_sprites={sprite_count} samples=0 expected_samples={DRAW_BENCHMARK_SAMPLES} complete=false outcome=no-presented-frames"
            );
            continue;
        }
        samples.sort_unstable();
        let p95_index = (samples.len() * 95).div_ceil(100).saturating_sub(1);
        eprintln!(
            "draw_baseline_sprites={sprite_count} samples={} expected_samples={DRAW_BENCHMARK_SAMPLES} complete={} median_us={:.2} p95_us={:.2}",
            samples.len(),
            samples.len() == DRAW_BENCHMARK_SAMPLES,
            samples[samples.len() / 2].as_secs_f64() * 1_000_000.0,
            samples[p95_index].as_secs_f64() * 1_000_000.0,
        );
    }
    Ok(())
}

fn run_smoke() -> Result<(), Box<dyn Error>> {
    let mut window: Option<WindowHandle> = None;
    let mut renderer: Option<Renderer> = None;
    let mut uploaded_texture: Option<TextureId> = None;
    let frames = Rc::new(Cell::new(0_u32));
    let frame_count = frames.clone();
    let error = Rc::new(Cell::new(None::<RenderError>));
    let render_error = error.clone();

    run_window(
        WindowConfig::new("Hycel 2D presentation smoke", 800, 450)?,
        move |event| match event {
            PlatformEvent::WindowCreated {
                window: created,
                metrics,
            } => {
                eprintln!(
                    "render_window={}x{} scale_factor={}",
                    metrics.surface_size.width, metrics.surface_size.height, metrics.scale_factor
                );
                match create_smoke_renderer(Arc::new(created.clone())) {
                    Ok((created_renderer, texture)) => {
                        renderer = Some(created_renderer);
                        uploaded_texture = Some(texture);
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
                let Some(texture) = uploaded_texture else {
                    return EventAction::Exit;
                };
                match render_smoke_frame(renderer, texture) {
                    Ok(FrameOutcome::Presented) => {
                        let next_count = frame_count.get() + 1;
                        frame_count.set(next_count);
                        if next_count == 1 || next_count == 30 {
                            eprintln!("presented_frames={next_count}");
                        }
                        if next_count >= 30 {
                            match benchmark_draw_workloads(renderer, texture) {
                                Ok(()) => EventAction::Exit,
                                Err(failure) => {
                                    render_error.set(Some(failure));
                                    EventAction::Exit
                                }
                            }
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
            PlatformEvent::Input(_) => EventAction::Continue,
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
