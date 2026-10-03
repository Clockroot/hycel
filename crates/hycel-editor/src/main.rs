use std::error::Error;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread;

use serde::Deserialize;

use hycel_editor::{EditorEntity, EditorSession, EditorTestReport};
use hycel_input::KeyCode;
use hycel_platform::{EventAction, PlatformEvent, WindowConfig, WindowHandle, run_window};
use hycel_project::SceneEditOperation;
use hycel_render::{Camera2D, DebugText, FrameOutcome, Renderer, Sprite, TextureId};

const MAX_CHILD_OUTPUT_LINE_BYTES: usize = 4_096;
const MAX_CHILD_OUTPUT_MESSAGE_CHARS: usize = 2_048;

fn main() {
    if let Err(error) = run() {
        eprintln!("hycel-editor: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let first = arguments
        .next()
        .ok_or("usage: hycel-editor [--smoke] <project-path>")?;
    let smoke_mode = first == "--smoke";
    let project = if smoke_mode {
        arguments
            .next()
            .ok_or("usage: hycel-editor --smoke <project-path>")?
    } else {
        first
    };
    if arguments.next().is_some() {
        return Err("usage: hycel-editor [--smoke] <project-path>".into());
    }
    let session = EditorSession::open(&PathBuf::from(project)).map_err(std::io::Error::other)?;
    let app = std::rc::Rc::new(std::cell::RefCell::new(EditorApp::new(session, smoke_mode)));
    let callback = app.clone();
    run_window(
        WindowConfig::new("Hycel Editor", 1280, 800)?.with_resizable(true),
        move |event| callback.borrow_mut().handle(event),
    )?;
    let (smoke_succeeded, smoke_status) = {
        let mut app = app.borrow_mut();
        app.stop_game();
        (
            matches!(app.smoke_state, SmokeState::Passed),
            app.status.clone(),
        )
    };
    if smoke_mode && !smoke_succeeded {
        return Err(format!("editor window smoke failed: {smoke_status}").into());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditorTextScale {
    OneX,
    TwoX,
}

struct EditorApp {
    session: EditorSession,
    window: Option<WindowHandle>,
    renderer: Option<Renderer>,
    status: String,
    preview_diff: Option<String>,
    preview_ready: bool,
    last_test_report: Option<EditorTestReport>,
    test_report_cursor: usize,
    test_report_message_offset: usize,
    test_report_viewer_open: bool,
    rename_buffer: Option<RenameBuffer>,
    resource_index: usize,
    animation_resource_index: usize,
    game: Option<Child>,
    runtime_output_tx: SyncSender<String>,
    runtime_output_rx: Receiver<String>,
    runtime_debug: Option<RuntimeDebugSnapshot>,
    runtime_log: Option<String>,
    text_scale: EditorTextScale,
    running: bool,
    smoke_state: SmokeState,
}

#[derive(Clone, Copy)]
enum RenameTarget {
    Entity,
    Scene,
    Tag,
}

struct RenameBuffer {
    target: RenameTarget,
    text: String,
}

#[allow(clippy::struct_excessive_bools)] // Snapshot fields describe orthogonal runtime flags.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RuntimeDebugSnapshot {
    kind: String,
    schema_version: u32,
    tick: u64,
    scene_tick: u64,
    scene_id: String,
    scene_name: String,
    player_position_milli: [i64; 2],
    player_velocity_milli: [i64; 2],
    grounded: bool,
    move_axis: i16,
    jump_pressed: bool,
    echo_pressed: bool,
    restart_pressed: bool,
    echo_remaining_ticks: usize,
    gate_open: bool,
    collected_chime_count: usize,
    completed: bool,
}

#[derive(Clone, Copy)]
enum SmokeState {
    Disabled,
    Active { presented: u32, attempts: u32 },
    Passed,
}

impl EditorApp {
    fn new(session: EditorSession, smoke_mode: bool) -> Self {
        let (runtime_output_tx, runtime_output_rx) = mpsc::sync_channel(64);
        Self {
            session,
            window: None,
            renderer: None,
            status: "Ready - arrows select, G tags, T/A animation, P preview, S save".to_owned(),
            preview_diff: None,
            preview_ready: false,
            last_test_report: None,
            test_report_cursor: 0,
            test_report_message_offset: 0,
            test_report_viewer_open: false,
            rename_buffer: None,
            resource_index: 0,
            animation_resource_index: 0,
            game: None,
            runtime_output_tx,
            runtime_output_rx,
            runtime_debug: None,
            runtime_log: None,
            text_scale: EditorTextScale::TwoX,
            running: false,
            smoke_state: if smoke_mode {
                SmokeState::Active {
                    presented: 0,
                    attempts: 0,
                }
            } else {
                SmokeState::Disabled
            },
        }
    }

    fn handle(&mut self, event: PlatformEvent) -> EventAction {
        self.drain_runtime_output();
        match event {
            PlatformEvent::WindowCreated { window, metrics } => {
                let _ = metrics;
                match Renderer::new(Arc::new(window.clone())) {
                    Ok(renderer) => {
                        self.renderer = Some(renderer);
                        self.window = Some(window.clone());
                        window.request_redraw();
                        EventAction::Continue
                    }
                    Err(error) => {
                        self.status = format!("Renderer unavailable: {error}");
                        EventAction::Exit
                    }
                }
            }
            PlatformEvent::Input(input) => {
                if let hycel_input::InputEvent::Key {
                    code,
                    pressed: true,
                    synthetic: false,
                } = input
                {
                    self.key_pressed(code);
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
                EventAction::Continue
            }
            PlatformEvent::FocusChanged(_) | PlatformEvent::ScaleFactorChanged(_) => {
                EventAction::Continue
            }
            PlatformEvent::Resized(size) => {
                if let Some(renderer) = &mut self.renderer {
                    if let Err(error) = renderer.resize(size) {
                        self.status = format!("Resize failed: {error}");
                    }
                }
                EventAction::Continue
            }
            PlatformEvent::RedrawRequested => {
                let outcome = self.render();
                if matches!(self.smoke_state, SmokeState::Active { .. }) {
                    return self.record_smoke_frame(outcome);
                }
                EventAction::Continue
            }
            PlatformEvent::CloseRequested => {
                self.stop_game();
                EventAction::Exit
            }
        }
    }

    fn record_smoke_frame(&mut self, outcome: Option<FrameOutcome>) -> EventAction {
        let SmokeState::Active {
            mut presented,
            mut attempts,
        } = self.smoke_state
        else {
            return EventAction::Continue;
        };
        attempts = attempts.saturating_add(1);
        match outcome {
            Some(FrameOutcome::Presented) => presented = presented.saturating_add(1),
            Some(_) => {}
            None => return EventAction::Exit,
        }
        if presented >= 30 {
            self.smoke_state = SmokeState::Passed;
            "Editor presented 30 frames successfully.".clone_into(&mut self.status);
            return EventAction::Exit;
        }
        if attempts >= 120 {
            self.status =
                format!("Only {presented} of 30 editor frames were presented after 120 attempts.");
            return EventAction::Exit;
        }
        self.smoke_state = SmokeState::Active {
            presented,
            attempts,
        };
        if let Some(window) = &self.window {
            window.request_redraw();
        }
        EventAction::Continue
    }

    fn key_pressed(&mut self, key: KeyCode) {
        if key == KeyCode::F10 {
            self.text_scale = match self.text_scale {
                EditorTextScale::OneX => EditorTextScale::TwoX,
                EditorTextScale::TwoX => EditorTextScale::OneX,
            };
            self.status = format!(
                "Editor text scale: {}x.",
                if self.text_scale == EditorTextScale::TwoX {
                    2
                } else {
                    1
                }
            );
            return;
        }
        if self.test_report_viewer_open {
            self.handle_test_report_key(key);
            return;
        }
        if self.rename_buffer.is_some() {
            self.handle_name_entry_key(key);
            return;
        }
        match key {
            KeyCode::Escape => "Close the editor window to exit.".clone_into(&mut self.status),
            KeyCode::Tab => match self.session.select_next_scene(false) {
                Ok(()) => "Scene changed.".clone_into(&mut self.status),
                Err(error) => self.status = error,
            },
            KeyCode::ArrowDown => {
                if self.session.is_dirty() {
                    "Save or discard staged edits before changing selection."
                        .clone_into(&mut self.status);
                } else if self.session.select_next_entity(false) {
                    "Next entity selected.".clone_into(&mut self.status);
                }
            }
            KeyCode::ArrowUp => {
                if self.session.is_dirty() {
                    "Save or discard staged edits before changing selection."
                        .clone_into(&mut self.status);
                } else if self.session.select_next_entity(true) {
                    "Previous entity selected.".clone_into(&mut self.status);
                }
            }
            KeyCode::KeyJ => self.stage_move([-25, 0]),
            KeyCode::KeyL => self.stage_move([25, 0]),
            KeyCode::KeyI => self.stage_move([0, -25]),
            KeyCode::KeyK => self.stage_move([0, 25]),
            KeyCode::KeyC
            | KeyCode::KeyA
            | KeyCode::KeyX
            | KeyCode::KeyV
            | KeyCode::KeyN
            | KeyCode::KeyM
            | KeyCode::KeyG
            | KeyCode::KeyT
            | KeyCode::KeyZ => {
                self.handle_authoring_key(key);
            }
            KeyCode::KeyP => self.preview(),
            KeyCode::KeyS => self.save_or_preview(),
            KeyCode::KeyR => match self.session.discard() {
                Ok(()) => {
                    self.preview_ready = false;
                    self.preview_diff = None;
                    "Unsaved changes discarded; reloaded from disk.".clone_into(&mut self.status);
                }
                Err(error) => self.status = error,
            },
            KeyCode::F5 => self.toggle_game(),
            KeyCode::F6 => match self.session.check() {
                Ok(()) => "Project validation passed.".clone_into(&mut self.status),
                Err(error) => self.status = format!("Validation failed: {error}"),
            },
            KeyCode::F7 => self.run_tests(),
            KeyCode::F8 => match self.session.export_screenshot() {
                Ok(path) => {
                    self.status = format!("Authored-scene SVG exported to {}.", path.display());
                }
                Err(error) => self.status = format!("Scene preview failed: {error}"),
            },
            KeyCode::F9 => self.inspect_next_resource(),
            _ => {}
        }
    }

    fn handle_name_entry_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Enter => {
                let Some(buffer) = self.rename_buffer.take() else {
                    return;
                };
                let result = match buffer.target {
                    RenameTarget::Entity => self.session.stage_rename(buffer.text),
                    RenameTarget::Scene => self.session.stage_rename_scene(buffer.text),
                    RenameTarget::Tag => self
                        .session
                        .stage_toggle_entity_tag(buffer.text.to_ascii_lowercase()),
                };
                match result {
                    Ok(()) => self.reset_preview("Edit staged; press P to preview."),
                    Err(error) => self.status = error,
                }
            }
            KeyCode::Backspace => {
                if let Some(buffer) = &mut self.rename_buffer {
                    buffer.text.pop();
                }
            }
            KeyCode::Escape => self.rename_buffer = None,
            key => {
                if let Some(character) = key_character(key) {
                    if let Some(buffer) = &mut self.rename_buffer {
                        let maximum = if matches!(buffer.target, RenameTarget::Tag) {
                            64
                        } else {
                            128
                        };
                        if buffer.text.chars().count() < maximum {
                            buffer.text.push(character);
                        }
                    }
                }
            }
        }
    }

    fn handle_test_report_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Escape => {
                self.test_report_viewer_open = false;
                "Closed test report viewer.".clone_into(&mut self.status);
            }
            KeyCode::ArrowUp => {
                self.test_report_cursor = self.test_report_cursor.saturating_sub(1);
                self.test_report_message_offset = 0;
            }
            KeyCode::ArrowDown => {
                if let Some(report) = &self.last_test_report {
                    self.test_report_cursor = self
                        .test_report_cursor
                        .saturating_add(1)
                        .min(report.tests.len().saturating_sub(1));
                    self.test_report_message_offset = 0;
                }
            }
            KeyCode::ArrowLeft => {
                self.test_report_message_offset =
                    self.test_report_message_offset.saturating_sub(220);
            }
            KeyCode::ArrowRight => {
                if let Some(report) = &self.last_test_report {
                    if !report.tests.is_empty() {
                        let index = self.test_report_cursor.min(report.tests.len() - 1);
                        let message_len = report.tests[index].message.chars().count();
                        let max_offset = message_len.saturating_sub(1) / 220 * 220;
                        self.test_report_message_offset = self
                            .test_report_message_offset
                            .saturating_add(220)
                            .min(max_offset);
                    }
                }
            }
            _ => {}
        }
    }

    fn show_test_report(&mut self, report: EditorTestReport) {
        self.test_report_cursor = report
            .tests
            .iter()
            .position(|test| !test.passed)
            .unwrap_or(0);
        self.test_report_message_offset = 0;
        self.last_test_report = Some(report);
        self.test_report_viewer_open = true;
    }

    fn run_tests(&mut self) {
        match self.session.run_tests_detailed() {
            Ok(report) => {
                let summary = format!(
                    "Reference-game tests: {} passed, {} failed.",
                    report.passed_count, report.failed_count
                );
                self.status = match self.session.export_test_report(&report) {
                    Ok(path) => format!("{summary} Report: {}", path.display()),
                    Err(error) => format!("{summary} Report export warning: {error}"),
                };
                self.show_test_report(report);
            }
            Err(error) => {
                self.last_test_report = None;
                self.test_report_viewer_open = false;
                self.status = format!("Reference-game tests failed: {error}");
            }
        }
    }

    fn select_next_animation_resource(&mut self) {
        if self.session.is_dirty() {
            "Save or discard staged edits before changing resource selection."
                .clone_into(&mut self.status);
            return;
        }
        let count = self.session.animation_resources().count();
        if count == 0 {
            "Project has no animation resource to select.".clone_into(&mut self.status);
            return;
        }
        self.animation_resource_index = (self.animation_resource_index + 1) % count;
        if let Some(resource) = self
            .session
            .animation_resources()
            .nth(self.animation_resource_index)
        {
            self.status = format!(
                "Animation resource {}/{}: {}.",
                self.animation_resource_index + 1,
                count,
                short(&resource.source, 64)
            );
        }
    }

    fn inspect_next_resource(&mut self) {
        let resources = self.session.resources();
        if resources.is_empty() {
            "Project has no registered resources.".clone_into(&mut self.status);
            return;
        }
        let inspected_index = self.resource_index % resources.len();
        let resource = &resources[inspected_index];
        self.resource_index = (inspected_index + 1) % resources.len();
        match self.session.inspect_resource_import(inspected_index) {
            Ok(status) if status.records.is_empty() => {
                self.status = format!(
                    "Asset {}/{} {} | SHA {} | no cached import record.",
                    inspected_index + 1,
                    resources.len(),
                    short(&resource.source, 22),
                    &status.source_sha256[..8]
                );
            }
            Ok(status) => {
                let record = &status.records[0];
                self.status = format!(
                    "Asset {}/{} {} | src {} | {} cache(s) | {} {:?} at recorded v{} | out {}.",
                    inspected_index + 1,
                    resources.len(),
                    short(&resource.source, 18),
                    &status.source_sha256[..8],
                    status.records.len(),
                    &record.fingerprint_sha256[..8],
                    record.decision_using_recorded_version,
                    short(&record.importer_version, 8),
                    if record.has_derived_output {
                        "present"
                    } else {
                        "missing"
                    }
                );
            }
            Err(error) => self.status = format!("Asset import inspection failed: {error}"),
        }
    }

    fn handle_authoring_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::KeyZ => match self.session.stage_create_scene("New Room".to_owned()) {
                Ok(scene_id) => {
                    self.reset_preview(&format!(
                        "New scene {scene_id} staged; press P to preview."
                    ));
                }
                Err(error) => self.status = error,
            },
            KeyCode::KeyC => match self.session.stage_create_entity("New Entity".to_owned()) {
                Ok(entity_id) => {
                    self.status = format!("New entity {entity_id} staged; press P to preview.");
                    self.preview_ready = false;
                }
                Err(error) => self.status = error,
            },
            KeyCode::KeyA => match self
                .session
                .stage_animation_component_at(self.animation_resource_index)
            {
                Ok(()) => self.reset_preview("Animation component staged; press P to preview."),
                Err(error) => self.status = error,
            },
            KeyCode::KeyX => match self.session.stage_delete_entity() {
                Ok(()) => self.reset_preview("Entity deletion staged; press P to preview."),
                Err(error) => self.status = error,
            },
            KeyCode::KeyV => {
                let component = self
                    .session
                    .selected_entity()
                    .and_then(|entity| entity.component_types.first().cloned());
                if let Some(component) = component {
                    match self.session.stage_remove_component(&component) {
                        Ok(()) => {
                            self.reset_preview("Component removal staged; press P to preview.");
                        }
                        Err(error) => self.status = error,
                    }
                } else {
                    "Selected entity has no component to remove.".clone_into(&mut self.status);
                }
            }
            KeyCode::KeyN => {
                if self.session.is_dirty() {
                    "Save or discard staged edits before starting another operation."
                        .clone_into(&mut self.status);
                } else if let Some(entity) = self.session.selected_entity() {
                    self.rename_buffer = Some(RenameBuffer {
                        target: RenameTarget::Entity,
                        text: entity.name.clone(),
                    });
                    "Rename entity: type letters/digits, Enter stages, Esc cancels."
                        .clone_into(&mut self.status);
                }
            }
            KeyCode::KeyT => self.select_next_animation_resource(),
            KeyCode::KeyG => {
                if self.session.is_dirty() {
                    "Save or discard staged edits before starting another operation."
                        .clone_into(&mut self.status);
                } else if self.session.selected_entity().is_some() {
                    self.rename_buffer = Some(RenameBuffer {
                        target: RenameTarget::Tag,
                        text: String::new(),
                    });
                    "Toggle tag: type letters/digits, Enter stages, Esc cancels."
                        .clone_into(&mut self.status);
                }
            }
            KeyCode::KeyM => {
                if self.session.is_dirty() {
                    "Save or discard staged edits before starting another operation."
                        .clone_into(&mut self.status);
                } else {
                    self.rename_buffer = Some(RenameBuffer {
                        target: RenameTarget::Scene,
                        text: self.session.current_scene().name.clone(),
                    });
                    "Rename scene: type letters/digits, Enter stages, Esc cancels."
                        .clone_into(&mut self.status);
                }
            }
            _ => {}
        }
    }

    fn stage_move(&mut self, delta: [i64; 2]) {
        match self.session.stage_move(delta) {
            Ok(()) => self.reset_preview("Transform staged; press P to preview."),
            Err(error) => self.status = error,
        }
    }

    fn reset_preview(&mut self, message: &str) {
        self.preview_ready = false;
        self.preview_diff = None;
        message.clone_into(&mut self.status);
    }

    fn preview(&mut self) {
        match self.session.preview() {
            Ok(preview) => {
                self.preview_ready = true;
                self.preview_diff = Some(preview.diff);
                self.status = format!(
                    "Preview ready | source {} | candidate {} | press S to confirm",
                    &preview.original_sha256[..8],
                    &preview.candidate_sha256[..8]
                );
            }
            Err(error) => self.status = format!("Preview failed: {error}"),
        }
    }

    fn save_or_preview(&mut self) {
        if !self.session.is_dirty() {
            "Nothing to save.".clone_into(&mut self.status);
        } else if !self.preview_ready {
            self.preview();
        } else {
            match self.session.apply_preview() {
                Ok(receipt) => {
                    self.preview_ready = false;
                    self.preview_diff = None;
                    self.status = receipt.backup_path.map_or_else(
                        || {
                            "Scene created transactionally; no prior file needed a backup."
                                .to_owned()
                        },
                        |backup| format!("Saved transactionally; backup {backup}"),
                    );
                }
                Err(error) => {
                    self.preview_ready = false;
                    self.status = format!("Save failed; preview again: {error}");
                }
            }
        }
    }

    fn toggle_game(&mut self) {
        if self.game.is_some() {
            self.stop_game();
            return;
        }
        let Some(binary) = editor_engine_binary() else {
            "Build/install the sibling `hycel` executable to launch Play."
                .clone_into(&mut self.status);
            return;
        };
        let mut command = Command::new(binary);
        command
            .arg("run")
            .arg(self.session.project_root())
            .env("HYCEL_EDITOR_DEBUG_STREAM", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match command.spawn() {
            Ok(mut child) => {
                if let Some(stdout) = child.stdout.take() {
                    let sender = self.runtime_output_tx.clone();
                    thread::spawn(move || forward_child_output(stdout, false, &sender));
                }
                if let Some(stderr) = child.stderr.take() {
                    let sender = self.runtime_output_tx.clone();
                    thread::spawn(move || forward_child_output(stderr, true, &sender));
                }
                self.runtime_debug = None;
                self.runtime_log = None;
                self.game = Some(child);
                self.running = true;
                "Play: launched reference runtime with bounded live debug telemetry."
                    .clone_into(&mut self.status);
            }
            Err(error) => self.status = format!("Play could not start: {error}"),
        }
    }

    fn stop_game(&mut self) {
        if let Some(mut child) = self.game.take() {
            if !matches!(child.try_wait(), Ok(Some(_))) {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        if self.running {
            self.running = false;
            "Stop: game process ended.".clone_into(&mut self.status);
        }
    }

    fn drain_runtime_output(&mut self) {
        while let Ok(line) = self.runtime_output_rx.try_recv() {
            if let Some(snapshot) = parse_runtime_debug_snapshot(&line) {
                self.runtime_debug = Some(snapshot);
            } else if !line.trim().is_empty() {
                self.runtime_log = Some(short(line.trim(), 90));
            }
        }
        let process_exited = self
            .game
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(Some(_))));
        if process_exited {
            self.game = None;
            self.running = false;
            "Play: runtime process exited.".clone_into(&mut self.status);
        }
    }

    fn render(&mut self) -> Option<FrameOutcome> {
        let texts = self.overlay_text();
        let Some(renderer) = &mut self.renderer else {
            return None;
        };
        let scene = self.session.current_scene();
        let selected = self.session.selected_entity();
        let mut sprites = vec![
            panel_sprite([-1.05, 0.0], [0.52, 1.7], [0.09, 0.13, 0.2, 1.0]),
            panel_sprite([0.0, 0.0], [1.03, 1.7], [0.07, 0.1, 0.16, 1.0]),
            panel_sprite([1.05, 0.0], [0.52, 1.7], [0.09, 0.13, 0.2, 1.0]),
        ];
        for (index, entity) in scene.entities.iter().enumerate() {
            let position = staged_position(&self.session, entity);
            let x = bounded_coordinate(position[0], 650.0).clamp(-0.85, 0.85);
            let y = 0.1 + bounded_coordinate(position[1], 1_200.0).clamp(-0.55, 0.55);
            let scale_x = bounded_scale(entity.scale_milli[0]);
            let scale_y = bounded_scale(entity.scale_milli[1]);
            let size = [scale_x.clamp(0.025, 0.22), scale_y.clamp(0.025, 0.22)];
            if selected.is_some_and(|selected| selected.id == entity.id) {
                let mut outline =
                    Sprite::new(TextureId::WHITE, [x, y], [size[0] + 0.025, size[1] + 0.025]);
                outline.layer = 1;
                outline.tint = [1.0, 0.84, 0.38, 1.0];
                sprites.push(outline);
            }
            let mut body = Sprite::new(TextureId::WHITE, [x, y], size);
            body.layer = 2;
            body.order = i32::try_from(index).unwrap_or(i32::MAX);
            body.tint = entity_color(entity);
            sprites.push(body);
        }
        match renderer.render_scene(Camera2D::default(), &sprites, &texts) {
            Ok(outcome) => Some(outcome),
            Err(error) => {
                self.status = format!("Render error: {error}");
                None
            }
        }
    }

    fn test_report_overlay_text(&self) -> Vec<DebugText> {
        let mut texts = Vec::new();
        add_text(
            &mut texts,
            22,
            20,
            "HYCEL TEST REPORT",
            [0.55, 0.78, 1.0, 1.0],
        );
        let Some(report) = &self.last_test_report else {
            add_text(
                &mut texts,
                22,
                60,
                "No test report is available.",
                [1.0, 0.78, 0.43, 1.0],
            );
            add_text(&mut texts, 22, 735, "Esc close", [0.62, 0.7, 0.82, 1.0]);
            return texts;
        };
        add_text(
            &mut texts,
            22,
            48,
            &format!(
                "{} passed | {} failed",
                report.passed_count, report.failed_count
            ),
            [0.84, 0.91, 1.0, 1.0],
        );
        if report.tests.is_empty() {
            add_text(
                &mut texts,
                22,
                80,
                "No named scenarios in this report.",
                [1.0, 0.78, 0.43, 1.0],
            );
            add_text(&mut texts, 22, 735, "Esc close", [0.62, 0.7, 0.82, 1.0]);
            return texts;
        }
        let index = self.test_report_cursor.min(report.tests.len() - 1);
        let test = &report.tests[index];
        let message_len = test.message.chars().count();
        let message_pages = message_len.max(1).div_ceil(220);
        add_text(
            &mut texts,
            22,
            84,
            &format!(
                "SCENARIO {}/{} | DETAIL {}/{} | UP/DOWN case | LEFT/RIGHT detail | Esc close",
                index + 1,
                report.tests.len(),
                self.test_report_message_offset / 220 + 1,
                message_pages
            ),
            [0.55, 0.78, 1.0, 1.0],
        );
        add_text(
            &mut texts,
            22,
            125,
            &format!(
                "{} | {}",
                if test.passed { "PASS" } else { "FAIL" },
                test.name
            ),
            if test.passed {
                [0.56, 0.9, 0.76, 1.0]
            } else {
                [1.0, 0.48, 0.45, 1.0]
            },
        );
        let first_line = test
            .message
            .chars()
            .skip(self.test_report_message_offset)
            .take(110)
            .collect::<String>();
        let second_line = test
            .message
            .chars()
            .skip(self.test_report_message_offset.saturating_add(110))
            .take(110)
            .collect::<String>();
        add_text(&mut texts, 22, 155, &first_line, [0.88, 0.9, 0.95, 1.0]);
        if !second_line.is_empty() {
            add_text(&mut texts, 22, 180, &second_line, [0.88, 0.9, 0.95, 1.0]);
        }
        add_text(
            &mut texts,
            22,
            735,
            "UP/DOWN browse cases | LEFT/RIGHT page details | Esc close",
            [0.62, 0.7, 0.82, 1.0],
        );
        texts
    }

    #[allow(clippy::too_many_lines)] // Keep the compact three-panel information layout together.
    fn overlay_text(&self) -> Vec<DebugText> {
        if self.test_report_viewer_open {
            let mut texts = self.test_report_overlay_text();
            self.apply_text_scale(&mut texts);
            return texts;
        }
        let scene = self.session.current_scene();
        let selected_index = self.session.selected_entity_index();
        let mut texts = Vec::new();
        add_text(
            &mut texts,
            20,
            20,
            &format!("HYCEL EDITOR  |  {}", self.session.project_name()),
            [0.84, 0.91, 1.0, 1.0],
        );
        add_text(
            &mut texts,
            20,
            58,
            &format!(
                "SCENE {}/{}  {}  |  {} entities  |  {} resources",
                self.session.scene_index() + 1,
                self.session.scenes().len(),
                scene.name,
                scene.entities.len(),
                self.session.resource_count()
            ),
            [0.4, 0.88, 0.8, 1.0],
        );
        add_text(&mut texts, 22, 102, "HIERARCHY", [0.95, 0.72, 0.42, 1.0]);
        let first = selected_index.saturating_sub(6);
        for (row, (actual, entity)) in scene
            .entities
            .iter()
            .enumerate()
            .skip(first)
            .take(14)
            .enumerate()
        {
            let marker = if actual == selected_index { "+" } else { " " };
            add_text(
                &mut texts,
                22,
                132 + u32::try_from(row).unwrap_or(0) * 28,
                &format!("{marker} {}", short(&entity.name, 21)),
                if actual == selected_index {
                    [1.0, 0.84, 0.38, 1.0]
                } else {
                    [0.72, 0.79, 0.9, 1.0]
                },
            );
        }
        add_text(
            &mut texts,
            365,
            102,
            "VIEWPORT | ORTHOGRAPHIC SCENE OVERVIEW",
            [0.55, 0.78, 1.0, 1.0],
        );
        add_text(&mut texts, 858, 102, "INSPECTOR", [0.95, 0.72, 0.42, 1.0]);
        if let Some(entity) = self.session.selected_entity() {
            add_text(
                &mut texts,
                858,
                136,
                &short(&entity.name, 30),
                [0.94, 0.95, 0.99, 1.0],
            );
            add_text(
                &mut texts,
                858,
                164,
                &format!("id {}", &entity.id[..entity.id.len().min(16)]),
                [0.57, 0.66, 0.78, 1.0],
            );
            let position = staged_position(&self.session, entity);
            add_text(
                &mut texts,
                858,
                205,
                "TRANSLATION (milli-units)",
                [0.55, 0.78, 1.0, 1.0],
            );
            add_text(
                &mut texts,
                858,
                235,
                &format!("x {:>7}    y {:>7}", position[0], position[1]),
                [0.84, 0.91, 1.0, 1.0],
            );
            add_text(&mut texts, 858, 276, "COMPONENTS", [0.55, 0.78, 1.0, 1.0]);
            if !entity.tags.is_empty() {
                add_text(
                    &mut texts,
                    858,
                    510,
                    &format!("TAGS {}", short(&entity.tags.join(", "), 24)),
                    [0.84, 0.75, 0.52, 1.0],
                );
            }
            for (index, component) in entity.component_types.iter().take(8).enumerate() {
                add_text(
                    &mut texts,
                    858,
                    306 + u32::try_from(index).unwrap_or(0) * 26,
                    &short(component, 28),
                    [0.72, 0.79, 0.9, 1.0],
                );
            }
        } else {
            add_text(
                &mut texts,
                858,
                136,
                "No entity selected",
                [0.72, 0.79, 0.9, 1.0],
            );
        }
        add_text(
            &mut texts,
            858,
            540,
            "ASSETS / DIRECT DEPENDENCIES",
            [0.55, 0.78, 1.0, 1.0],
        );
        for (index, resource) in self.session.resources().iter().take(2).enumerate() {
            add_text(
                &mut texts,
                858,
                564 + u32::try_from(index).unwrap_or(0) * 20,
                &format!(
                    "{} | {} | {} edges",
                    short(&resource.source, 20),
                    resource.kind,
                    resource.dependency_count
                ),
                [0.72, 0.79, 0.9, 1.0],
            );
        }
        if let Some(buffer) = &self.rename_buffer {
            add_text(
                &mut texts,
                22,
                565,
                &format!("RENAME: {}_", short(&buffer.text, 34)),
                [1.0, 0.84, 0.38, 1.0],
            );
        }
        if let Some(snapshot) = &self.runtime_debug {
            add_text(
                &mut texts,
                365,
                620,
                &format!(
                    "RUNTIME t{} r{} | {} [{}]",
                    snapshot.tick,
                    snapshot.scene_tick,
                    short(&snapshot.scene_name, 10),
                    short(&snapshot.scene_id, 8),
                ),
                [0.56, 0.9, 0.76, 1.0],
            );
            add_text(
                &mut texts,
                365,
                642,
                &format!(
                    "POS {},{} | VEL {},{} | {}",
                    snapshot.player_position_milli[0],
                    snapshot.player_position_milli[1],
                    snapshot.player_velocity_milli[0],
                    snapshot.player_velocity_milli[1],
                    if snapshot.grounded {
                        "grounded"
                    } else {
                        "airborne"
                    },
                ),
                [0.84, 0.91, 1.0, 1.0],
            );
            add_text(
                &mut texts,
                365,
                664,
                &format!(
                    "INPUT axis {} | J{} E{} R{}",
                    snapshot.move_axis,
                    if snapshot.jump_pressed { "+" } else { "-" },
                    if snapshot.echo_pressed { "+" } else { "-" },
                    if snapshot.restart_pressed { "+" } else { "-" },
                ),
                [0.84, 0.91, 1.0, 1.0],
            );
            add_text(
                &mut texts,
                365,
                686,
                &format!(
                    "ECHO {} | GATE {} | BELLS {}{}",
                    snapshot.echo_remaining_ticks,
                    if snapshot.gate_open { "open" } else { "shut" },
                    snapshot.collected_chime_count,
                    if snapshot.completed { " | done" } else { "" },
                ),
                [0.84, 0.91, 1.0, 1.0],
            );
        }
        if let Some(message) = &self.runtime_log {
            add_text(
                &mut texts,
                858,
                642,
                &format!("LOG | {}", short(message, 28)),
                [0.84, 0.75, 0.52, 1.0],
            );
        }
        if let Some(report) = &self.last_test_report {
            add_text(
                &mut texts,
                22,
                583,
                &format!(
                    "LAST TEST REPORT | {} passed | {} failed | F7 reruns tests",
                    report.passed_count, report.failed_count
                ),
                [0.55, 0.78, 1.0, 1.0],
            );
        } else if let Some(diff) = &self.preview_diff {
            for (index, line) in diff.lines().take(3).enumerate() {
                add_text(
                    &mut texts,
                    22,
                    605 + u32::try_from(index).unwrap_or(0) * 22,
                    &short(line, 110),
                    [0.56, 0.9, 0.76, 1.0],
                );
            }
        }
        add_text(
            &mut texts,
            22,
            700,
            &short(&self.status, 94),
            [1.0, 0.78, 0.43, 1.0],
        );
        add_text(
            &mut texts,
            22,
            735,
            "Tab scene | UP/DOWN select | IJKL move | Z new scene",
            [0.62, 0.7, 0.82, 1.0],
        );
        add_text(
            &mut texts,
            22,
            755,
            "M rename scene | C entity | N rename | A animation | X delete | V remove",
            [0.62, 0.7, 0.82, 1.0],
        );
        add_text(
            &mut texts,
            22,
            775,
            "P preview | S save | R discard | F5 play | F6 check | F7 tests | F8 SVG | F9 asset | F10 text",
            [0.62, 0.7, 0.82, 1.0],
        );
        self.apply_text_scale(&mut texts);
        texts
    }

    fn apply_text_scale(&self, texts: &mut [DebugText]) {
        if self.text_scale == EditorTextScale::OneX {
            for text in texts {
                text.scale = 1;
            }
        }
    }
}

fn parse_runtime_debug_snapshot(line: &str) -> Option<RuntimeDebugSnapshot> {
    let snapshot = serde_json::from_str::<RuntimeDebugSnapshot>(line).ok()?;
    (snapshot.kind == "hycel_runtime_debug" && snapshot.schema_version == 1).then_some(snapshot)
}

fn forward_child_output<R: Read>(stream: R, is_stderr: bool, sender: &SyncSender<String>) {
    let mut reader = BufReader::new(stream);
    let mut line = Vec::with_capacity(256);
    let mut truncated = false;
    while let Ok(available) = reader.fill_buf() {
        if available.is_empty() {
            if !line.is_empty() || truncated {
                let _ = send_child_output_line(&line, truncated, is_stderr, sender);
            }
            break;
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let content_length = newline.unwrap_or(available.len());
        let remaining = MAX_CHILD_OUTPUT_LINE_BYTES.saturating_sub(line.len());
        let retained = content_length.min(remaining);
        line.extend_from_slice(&available[..retained]);
        truncated |= retained < content_length;
        let consumed = newline.map_or(content_length, |index| index + 1);
        reader.consume(consumed);
        if newline.is_some() {
            if !send_child_output_line(&line, truncated, is_stderr, sender) {
                break;
            }
            line.clear();
            truncated = false;
        }
    }
}

fn send_child_output_line(
    bytes: &[u8],
    truncated: bool,
    is_stderr: bool,
    sender: &SyncSender<String>,
) -> bool {
    let mut text = String::from_utf8_lossy(bytes).trim().to_owned();
    if truncated {
        text = format!("{} [line truncated]", short(&text, 2_000));
    }
    if is_stderr {
        text = format!("stderr: {text}");
    }
    match sender.try_send(short(&text, MAX_CHILD_OUTPUT_MESSAGE_CHARS)) {
        Ok(()) | Err(TrySendError::Full(_)) => true,
        Err(TrySendError::Disconnected(_)) => false,
    }
}

fn staged_position(session: &EditorSession, entity: &EditorEntity) -> [i64; 2] {
    match session.pending_operation() {
        Some(SceneEditOperation::SetEntityTransform {
            entity_id,
            translation_milli: Some(position),
            ..
        }) if entity_id == &entity.id => *position,
        _ => entity.translation_milli,
    }
}

fn panel_sprite(center: [f32; 2], size: [f32; 2], color: [f32; 4]) -> Sprite {
    let mut sprite = Sprite::new(TextureId::WHITE, center, size);
    sprite.tint = color;
    sprite
}

fn bounded_coordinate(value_milli: i64, divisor: f32) -> f32 {
    let bounded = value_milli.clamp(-10_000, 10_000);
    f32::from(i16::try_from(bounded).unwrap_or(0)) / divisor
}

fn bounded_scale(value_milli: i64) -> f32 {
    let bounded = value_milli.clamp(0, 4_000);
    f32::from(i16::try_from(bounded).unwrap_or(0)) / 4_000.0
}

fn entity_color(entity: &EditorEntity) -> [f32; 4] {
    let types = entity.component_types.join(" ");
    if types.contains("physics") || entity.name.to_lowercase().contains("ground") {
        [0.32, 0.53, 0.72, 1.0]
    } else if entity.name.to_lowercase().contains("hazard") {
        [0.9, 0.3, 0.37, 1.0]
    } else if entity.name.to_lowercase().contains("player") {
        [0.32, 0.78, 0.85, 1.0]
    } else {
        [0.66, 0.53, 0.87, 1.0]
    }
}

fn add_text(texts: &mut Vec<DebugText>, x: u32, y: u32, value: &str, color: [f32; 4]) {
    if texts.len() >= 40 {
        return;
    }
    let displayed_value = value.chars().take(120).collect::<String>();
    let max_double_scale_length = if x < 300 {
        94
    } else if x < 858 {
        40
    } else {
        34
    };
    let scale = if displayed_value.chars().count() <= max_double_scale_length {
        2
    } else {
        1
    };
    let mut text = DebugText::new([x, y], displayed_value);
    text.scale = scale;
    text.color = color;
    texts.push(text);
}

fn short(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

fn key_character(key: KeyCode) -> Option<char> {
    Some(match key {
        KeyCode::KeyA => 'A',
        KeyCode::KeyB => 'B',
        KeyCode::KeyC => 'C',
        KeyCode::KeyD => 'D',
        KeyCode::KeyE => 'E',
        KeyCode::KeyF => 'F',
        KeyCode::KeyG => 'G',
        KeyCode::KeyH => 'H',
        KeyCode::KeyI => 'I',
        KeyCode::KeyJ => 'J',
        KeyCode::KeyK => 'K',
        KeyCode::KeyL => 'L',
        KeyCode::KeyM => 'M',
        KeyCode::KeyN => 'N',
        KeyCode::KeyO => 'O',
        KeyCode::KeyP => 'P',
        KeyCode::KeyQ => 'Q',
        KeyCode::KeyR => 'R',
        KeyCode::KeyS => 'S',
        KeyCode::KeyT => 'T',
        KeyCode::KeyU => 'U',
        KeyCode::KeyV => 'V',
        KeyCode::KeyW => 'W',
        KeyCode::KeyX => 'X',
        KeyCode::KeyY => 'Y',
        KeyCode::KeyZ => 'Z',
        KeyCode::Digit0 => '0',
        KeyCode::Digit1 => '1',
        KeyCode::Digit2 => '2',
        KeyCode::Digit3 => '3',
        KeyCode::Digit4 => '4',
        KeyCode::Digit5 => '5',
        KeyCode::Digit6 => '6',
        KeyCode::Digit7 => '7',
        KeyCode::Digit8 => '8',
        KeyCode::Digit9 => '9',
        KeyCode::Space => ' ',
        _ => return None,
    })
}

fn editor_engine_binary() -> Option<PathBuf> {
    let current = std::env::current_exe().ok()?;
    let name = format!("hycel{}", std::env::consts::EXE_SUFFIX);
    let sibling = current.parent()?.join(name);
    sibling.is_file().then_some(sibling)
}

#[cfg(test)]
mod tests {
    use super::{
        EditorApp, MAX_CHILD_OUTPUT_LINE_BYTES, MAX_CHILD_OUTPUT_MESSAGE_CHARS,
        forward_child_output, parse_runtime_debug_snapshot,
    };
    use hycel_editor::{EditorSession, EditorTestCase, EditorTestReport};
    use hycel_input::KeyCode;
    use hycel_project::SceneEditOperation;
    use std::io::Cursor;
    use std::path::Path;
    use std::sync::mpsc;

    #[test]
    fn runtime_debug_stream_accepts_only_the_versioned_bounded_shape() {
        let valid = r#"{"kind":"hycel_runtime_debug","schema_version":1,"tick":12,"scene_tick":5,"scene_id":"scene-id","scene_name":"Room","player_position_milli":[10,20],"player_velocity_milli":[30,40],"grounded":true,"move_axis":1,"jump_pressed":false,"echo_pressed":false,"restart_pressed":false,"echo_remaining_ticks":0,"gate_open":false,"collected_chime_count":2,"completed":false}"#;
        let snapshot = parse_runtime_debug_snapshot(valid).unwrap();
        assert_eq!(snapshot.tick, 12);
        assert_eq!(snapshot.scene_tick, 5);
        assert_eq!(snapshot.player_position_milli, [10, 20]);
        assert!(snapshot.grounded);
        assert!(
            parse_runtime_debug_snapshot(
                &valid.replace("\"schema_version\":1", "\"schema_version\":2")
            )
            .is_none()
        );
        assert!(
            parse_runtime_debug_snapshot(
                &valid.replace("\"completed\":false", "\"completed\":false,\"extra\":true")
            )
            .is_none()
        );

        let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/platformer-game");
        let mut app = EditorApp::new(EditorSession::open(&project).unwrap(), false);
        app.runtime_output_tx.try_send(valid.to_owned()).unwrap();
        app.drain_runtime_output();
        assert_eq!(app.runtime_debug.as_ref().unwrap().tick, 12);
        let overlay = app.overlay_text();
        assert!(
            overlay
                .iter()
                .any(|text| text.text.contains("RUNTIME t12 r5"))
        );
        assert!(
            overlay
                .iter()
                .any(|text| text.text.contains("HYCEL EDITOR") && text.scale == 2)
        );
        assert!(overlay.len() <= 40);
    }

    #[test]
    fn f10_toggles_global_editor_text_scale_without_mutating_the_project() {
        let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/platformer-game");
        let session = EditorSession::open(&project).unwrap();
        let mut app = EditorApp::new(session, false);
        let scene_id = app.session.current_scene().id.clone();

        assert!(app.overlay_text().iter().any(|text| text.scale == 2));
        app.key_pressed(KeyCode::F10);
        assert!(app.overlay_text().iter().all(|text| text.scale == 1));
        assert!(app.status.contains("1x"));
        app.key_pressed(KeyCode::F10);
        assert!(app.overlay_text().iter().any(|text| text.scale == 2));
        assert!(app.status.contains("2x"));
        assert_eq!(app.session.current_scene().id, scene_id);
        assert!(!app.session.is_dirty());
    }

    #[test]
    fn runtime_output_forwarding_is_nonblocking_bounded_and_labels_stderr() {
        let (sender, receiver) = mpsc::sync_channel(1);
        forward_child_output(Cursor::new(b"first\nsecond\n"), false, &sender);
        assert_eq!(receiver.try_recv().unwrap(), "first");
        assert!(receiver.try_recv().is_err());

        let (sender, receiver) = mpsc::sync_channel(1);
        forward_child_output(Cursor::new(b"device warning\n"), true, &sender);
        assert_eq!(receiver.try_recv().unwrap(), "stderr: device warning");

        let (sender, receiver) = mpsc::sync_channel(1);
        let oversized_line = vec![b'x'; MAX_CHILD_OUTPUT_LINE_BYTES + 64 * 1024];
        forward_child_output(Cursor::new(oversized_line), false, &sender);
        let message = receiver.try_recv().unwrap();
        assert!(message.ends_with("[line truncated]"));
        assert!(message.chars().count() <= MAX_CHILD_OUTPUT_MESSAGE_CHARS);
    }

    #[test]
    fn test_report_viewer_browses_cases_and_long_messages_without_scene_mutation() {
        let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/platformer-game");
        let session = EditorSession::open(&project).unwrap();
        let scene_id = session.current_scene().id.clone();
        let mut app = EditorApp::new(session, false);
        app.last_test_report = Some(EditorTestReport {
            tests: vec![
                EditorTestCase {
                    name: "first".to_owned(),
                    passed: true,
                    message: "x".repeat(500),
                },
                EditorTestCase {
                    name: "echo-flight".to_owned(),
                    passed: false,
                    message: "gate stayed closed".to_owned(),
                },
            ],
            passed_count: 1,
            failed_count: 1,
        });
        app.test_report_viewer_open = true;

        app.key_pressed(KeyCode::ArrowRight);
        assert_eq!(app.test_report_message_offset, 220);
        app.key_pressed(KeyCode::ArrowDown);
        assert_eq!(app.test_report_cursor, 1);
        assert_eq!(app.test_report_message_offset, 0);
        let viewer_text = app.overlay_text();
        assert!(
            viewer_text
                .iter()
                .any(|text| text.text.contains("gate stayed closed"))
        );
        assert!(viewer_text.len() < 32);
        app.key_pressed(KeyCode::Tab);
        assert_eq!(app.session.current_scene().id, scene_id);
        app.key_pressed(KeyCode::Escape);
        assert!(!app.test_report_viewer_open);
        assert!(!app.session.is_dirty());

        app.test_report_message_offset = 220;
        app.show_test_report(EditorTestReport {
            tests: Vec::new(),
            passed_count: 0,
            failed_count: 0,
        });
        assert_eq!(app.test_report_message_offset, 0);
        app.key_pressed(KeyCode::ArrowRight);
        let empty_viewer = app.overlay_text();
        assert!(
            empty_viewer
                .iter()
                .any(|text| text.text.contains("No named scenarios"))
        );
        app.key_pressed(KeyCode::Escape);
    }

    #[test]
    fn g_key_stages_a_lowercase_tag_toggle() {
        let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/platformer-game");
        let session = EditorSession::open(&project).unwrap();
        let mut app = EditorApp::new(session, false);
        app.key_pressed(KeyCode::KeyG);
        for key in [
            KeyCode::KeyA,
            KeyCode::KeyM,
            KeyCode::KeyB,
            KeyCode::KeyE,
            KeyCode::KeyR,
        ] {
            app.key_pressed(key);
        }
        app.key_pressed(KeyCode::Enter);
        assert!(matches!(
            app.session.pending_operation(),
            Some(SceneEditOperation::SetEntityTags { tags, .. }) if tags.iter().any(|tag| tag == "amber")
        ));
    }

    #[test]
    fn t_cycles_animation_resources_and_a_stages_the_selected_component() {
        let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/platformer-game");
        let session = EditorSession::open(&project).unwrap();
        let mut app = EditorApp::new(session, false);
        app.key_pressed(KeyCode::KeyT);
        assert!(app.status.contains("Animation resource 1/1"));
        app.key_pressed(KeyCode::KeyA);
        assert!(matches!(
            app.session.pending_operation(),
            Some(SceneEditOperation::SetEntityComponent { data, .. })
                if data.get("clip_id").is_some_and(|value| value.as_str() == Some("30000000-0000-4000-8000-000000000002"))
        ));
    }

    #[test]
    fn m_key_stages_scene_rename_and_dirty_state_blocks_scene_switching() {
        let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/platformer-game");
        let session = EditorSession::open(&project).unwrap();
        let original_scene_id = session.current_scene().id.clone();
        let original_name = session.current_scene().name.clone();
        let mut app = EditorApp::new(session, false);

        app.key_pressed(KeyCode::KeyM);
        for _ in original_name.chars() {
            app.key_pressed(KeyCode::Backspace);
        }
        for key in [
            KeyCode::KeyR,
            KeyCode::KeyO,
            KeyCode::KeyO,
            KeyCode::KeyM,
            KeyCode::Digit7,
        ] {
            app.key_pressed(key);
        }
        app.key_pressed(KeyCode::Enter);

        assert!(matches!(
            app.session.pending_operation(),
            Some(SceneEditOperation::RenameScene { name }) if name == "ROOM7"
        ));
        app.key_pressed(KeyCode::Tab);
        assert!(app.session.is_dirty());
        assert_eq!(app.session.current_scene().id, original_scene_id);
        assert!(app.status.contains("save or discard"));
    }
}
