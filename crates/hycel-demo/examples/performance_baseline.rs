use std::{
    cell::Cell,
    error::Error,
    fmt::Write as FmtWrite,
    fs,
    hint::black_box,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use hycel_assets::hash_source_file;
use hycel_core::{ComponentStorage, World};
use hycel_input::{
    AxisBinding, ButtonBinding, FocusLossBehavior, InputBindings, InputControl, InputEvent,
    InputMapper, KeyCode,
};
use hycel_project::{ComponentRegistry, SceneDocument};

const SAMPLE_COUNT: usize = 100;
const WARMUP_COUNT: usize = 10;
type WorldFixture = (World, ComponentStorage<Cell<i64>>);

fn main() -> Result<(), Box<dyn Error>> {
    let report = generate_report()?;
    fs::create_dir_all("target")?;
    fs::write("target/performance-baseline-cpu.md", &report)?;
    print!("{report}");
    Ok(())
}

fn generate_report() -> Result<String, Box<dyn Error>> {
    let mut report = String::new();
    report.push_str("# Hycel CPU performance baseline\n\n");
    let parallelism = std::thread::available_parallelism()
        .map_or_else(|_| "unknown".to_owned(), |count| count.to_string());
    let _ = writeln!(
        report,
        "- OS/architecture: `{}/{}`\n- CPU: `{}`\n- Rust: `{}`\n- Available parallelism: `{parallelism}`\n- Measurements: `{SAMPLE_COUNT}` samples after `{WARMUP_COUNT}` warmups; process-local wall time\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
        cpu_name(),
        rustc_version(),
    );
    report.push_str("| Workload | Size | Median (µs) | P95 (µs) |\n|---|---:|---:|---:|\n");
    benchmark_world(&mut report)?;
    benchmark_assets(&mut report)?;
    benchmark_input(&mut report)?;
    report.push_str(
        "\nProxy only: synthetic normalized events are mapped to tick snapshots on the CPU. This does not measure OS event delivery, physical input, GPU presentation, or input-to-photon latency. Asset measurements use warm local file-cache reads and hash bytes only; they do not include format decoding or importer work.\n",
    );
    Ok(report)
}

fn benchmark_world(report: &mut String) -> Result<(), Box<dyn Error>> {
    for entity_count in [100, 1_000, 10_000] {
        let (world, positions) = make_world(entity_count)?;
        let mut tick = || {
            let mut checksum = 0_i64;
            for (_, position) in positions.iter(&world) {
                let next = position.get().wrapping_add(1);
                position.set(next);
                checksum = checksum.wrapping_add(next);
            }
            black_box(checksum)
        };
        let (median, p95) = measure(&mut tick);
        append_row(
            report,
            "world query + position tick",
            entity_count,
            median,
            p95,
        );
    }
    Ok(())
}

fn benchmark_assets(report: &mut String) -> Result<(), Box<dyn Error>> {
    let temporary_root = create_asset_fixture_root()?;
    for byte_count in [4 * 1024, 64 * 1024, 1024 * 1024] {
        let relative_path = format!("assets/source-{byte_count}.bin");
        let path = temporary_root.join(&relative_path);
        let bytes = (0..byte_count)
            .map(|index: usize| {
                u8::try_from(index.wrapping_mul(31) % 251).expect("value is below 251")
            })
            .collect::<Vec<_>>();
        fs::write(&path, bytes)?;
        let mut hash = || {
            let digest = hash_source_file(&temporary_root, &relative_path, 1024 * 1024)
                .expect("benchmark source file remains available");
            black_box(digest.to_hex())
        };
        let (median, p95) = measure(&mut hash);
        append_row(report, "source read + SHA-256", byte_count, median, p95);
    }
    fs::remove_dir_all(temporary_root)?;

    let scene_path = Path::new("examples/empty-project/scenes/first-room.json");
    let scene_bytes = fs::read(scene_path)?;
    let mut registry = ComponentRegistry::default();
    registry.register("hycel.animation", 1, false, ["clip_id"])?;
    let mut parse_scene = || {
        let scene = SceneDocument::parse_json_with_registry(
            &scene_bytes,
            "scenes/first-room.json",
            &registry,
        )
        .expect("example scene remains valid");
        black_box(scene)
    };
    let (median, p95) = measure(&mut parse_scene);
    append_row(
        report,
        "strict scene parse + validation",
        scene_bytes.len(),
        median,
        p95,
    );
    Ok(())
}

fn benchmark_input(report: &mut String) -> Result<(), Box<dyn Error>> {
    let bindings = InputBindings::new(
        FocusLossBehavior::ReleaseAll,
        vec![ButtonBinding::new(
            1,
            "jump",
            vec![InputControl::Key {
                code: KeyCode::Space,
            }],
        )],
        vec![AxisBinding::new(
            2,
            "move_x",
            vec![InputControl::Key {
                code: KeyCode::KeyA,
            }],
            vec![InputControl::Key {
                code: KeyCode::KeyD,
            }],
        )],
    )?;
    let mut mapper = InputMapper::new(bindings);
    let mut tick_index = 0_u64;
    let mut input_to_frame = || {
        mapper.handle_event(InputEvent::Key {
            code: KeyCode::KeyD,
            pressed: true,
            synthetic: false,
        });
        mapper.handle_event(InputEvent::Key {
            code: KeyCode::Space,
            pressed: true,
            synthetic: false,
        });
        let frame = mapper.frame(tick_index);
        mapper.handle_event(InputEvent::Key {
            code: KeyCode::KeyD,
            pressed: false,
            synthetic: false,
        });
        mapper.handle_event(InputEvent::Key {
            code: KeyCode::Space,
            pressed: false,
            synthetic: false,
        });
        tick_index += 1;
        black_box((frame.button(1), frame.axis(2)))
    };
    let (median, p95) = measure(&mut input_to_frame);
    append_row(
        report,
        "synthetic input event → tick snapshot",
        1,
        median,
        p95,
    );
    Ok(())
}

fn make_world(entity_count: usize) -> Result<WorldFixture, Box<dyn Error>> {
    let mut world = World::default();
    let mut positions = ComponentStorage::default();
    for _ in 0..entity_count {
        let entity = world.spawn()?;
        positions.insert(&world, entity, Cell::new(0))?;
    }
    Ok((world, positions))
}

fn measure<T>(mut workload: impl FnMut() -> T) -> (Duration, Duration) {
    for _ in 0..WARMUP_COUNT {
        black_box(workload());
    }
    let mut samples = Vec::with_capacity(SAMPLE_COUNT);
    for _ in 0..SAMPLE_COUNT {
        let start = Instant::now();
        black_box(workload());
        samples.push(start.elapsed());
    }
    samples.sort_unstable();
    let p95_index = (SAMPLE_COUNT * 95).div_ceil(100).saturating_sub(1);
    (samples[SAMPLE_COUNT / 2], samples[p95_index])
}

fn append_row(report: &mut String, name: &str, size: usize, median: Duration, p95: Duration) {
    let median_micros = median.as_secs_f64() * 1_000_000.0;
    let p95_micros = p95.as_secs_f64() * 1_000_000.0;
    let _ = writeln!(
        report,
        "| {name} | {size} | {median_micros:.2} | {p95_micros:.2} |"
    );
}

fn create_asset_fixture_root() -> Result<PathBuf, Box<dyn Error>> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let root = std::env::temp_dir().join(format!("hycel-perf-{}-{nonce}", std::process::id()));
    fs::create_dir_all(root.join("assets"))?;
    Ok(root)
}

fn cpu_name() -> String {
    #[cfg(target_os = "macos")]
    {
        Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map_or_else(
                || "unavailable".to_owned(),
                |output| String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            )
    }
    #[cfg(target_os = "linux")]
    {
        fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|contents| {
                contents.lines().find_map(|line| {
                    line.strip_prefix("model name\t: ")
                        .or_else(|| line.strip_prefix("Hardware\t: "))
                        .map(str::to_owned)
                })
            })
            .unwrap_or_else(|| "unavailable".to_owned())
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_else(|_| "unavailable".to_owned())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        "unavailable".to_owned()
    }
}

fn rustc_version() -> String {
    Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map_or_else(
            || "unavailable".to_owned(),
            |output| String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        )
}
