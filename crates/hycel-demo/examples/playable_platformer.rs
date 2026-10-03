//! Interactive two-room platformer showing the engine APIs working together.
//!
//! Run from the workspace root with `cargo run -p hycel-demo --example playable_platformer`.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use hycel_animation::{AnimationPlayer, SceneTransitionQueue};
use hycel_assets::{AssetDependencyReport, hash_source_file};
use hycel_audio::{AudioClip, AudioService, AudioStatus, PlaybackOutcome};
use hycel_core::{FixedClock, FrameAdvance, SimScalar, TimeScale, Vec2};
use hycel_input::{ActionEventTracker, ButtonActionEventKind, InputBindings, InputMapper};
use hycel_physics::{BodyId, BodyKind, BoxBody, PhysicsConfig, PhysicsWorld};
use hycel_platform::{
    EventAction, PlatformEvent, SurfaceSize, WindowConfig, WindowHandle, run_window,
};
use hycel_project::{
    AnimationClipDocument, ComponentRegistry, ProjectManifest, ResourceDescriptor,
    ResourceRegistry, SceneDocument,
};
use hycel_render::{Camera2D, DebugText, FrameOutcome, Renderer, RgbaImage, Sprite, TextureId};
use hycel_save::{GameProgress, ProgressStore, SaveError};

const CONTENT_ROOT: &str = "examples/platformer-game";
const MAX_SOURCE_BYTES: u64 = 64 * 1024;
const ACTION_MOVE_X: u16 = 0;
const ACTION_JUMP: u16 = 1;
const ACTION_RESTART: u16 = 2;
const PLAYER_SPEED_MILLI: i64 = 1_500;
const JUMP_SPEED_MILLI: i64 = -3_200;
const GRAVITY_MILLI: i64 = 9_800;
const WAVE_SAMPLE_RATE: u32 = 22_050;

struct Content {
    manifest: ProjectManifest,
    scenes: Vec<(String, SceneDocument)>,
    animation: AnimationClipDocument,
    input: InputBindings,
    texture_bytes: BTreeMap<String, Vec<u8>>,
}

fn load_content() -> Result<Content, Box<dyn Error>> {
    let root = find_content_root()?;
    let manifest_text = fs::read_to_string(root.join("hycel.toml"))?;
    let manifest = ProjectManifest::parse_toml(&manifest_text)?;

    let mut components = ComponentRegistry::default();
    components.register("hycel.animation", 1, false, ["clip_id"])?;
    components.mark_resource_reference("hycel.animation", "clip_id")?;

    let mut scenes = Vec::new();
    for file in ["scenes/first-room.json", "scenes/last-room.json"] {
        let bytes = fs::read(root.join(file))?;
        let scene = SceneDocument::parse_json_with_registry(&bytes, file, &components)
            .map_err(|diagnostics| format_diagnostics(&diagnostics))?;
        scenes.push((file.to_owned(), scene));
    }

    let animation_path = "assets/player-run.animation.json";
    let animation_bytes = fs::read(root.join(animation_path))?;
    let animation = AnimationClipDocument::parse_json(&animation_bytes, animation_path)
        .map_err(|diagnostics| format_diagnostics(&diagnostics))?;

    let mut resource_registry = ResourceRegistry::default();
    resource_registry
        .register("texture", std::iter::empty::<&str>())
        .and_then(|()| resource_registry.register("animation", std::iter::empty::<&str>()))?;
    let resource_files = [
        "assets/player-frame-1.hycel.json",
        "assets/player-frame-2.hycel.json",
        "assets/player-run.animation.hycel.json",
    ];
    let mut resources = Vec::new();
    let mut texture_bytes = BTreeMap::new();
    for file in resource_files {
        let bytes = fs::read(root.join(file))?;
        let descriptor =
            ResourceDescriptor::parse_json_with_registry(&bytes, file, &resource_registry)
                .map_err(|diagnostics| format_diagnostics(&diagnostics))?;
        if descriptor.kind() == "texture" {
            let source_hash = hash_source_file(&root, descriptor.source(), MAX_SOURCE_BYTES)?;
            let image = fs::read(root.join(descriptor.source()))?;
            if image.len() != 8 * 8 * 4 {
                return Err(format!(
                    "{} must contain exactly 8x8 RGBA pixels",
                    descriptor.source()
                )
                .into());
            }
            // Hashing at load keeps this example's source read bounded and makes the
            // authored resource bytes, rather than their path, the identity check.
            if source_hash.as_bytes() != hycel_assets::ContentHash::from_bytes(&image).as_bytes() {
                return Err(
                    format!("{} changed while it was being loaded", descriptor.source()).into(),
                );
            }
            texture_bytes.insert(descriptor.id().to_owned(), image);
        }
        resources.push((file.to_owned(), descriptor));
    }
    let report = AssetDependencyReport::build(
        &scenes,
        &resources,
        &components,
        &[(
            "30000000-0000-4000-8000-000000000002".to_owned(),
            animation.clone(),
        )],
    )?;
    // Ensure both authored scenes and the animation are actually connected by UUID.
    if report.scenes().len() != scenes.len() {
        return Err("sample project dependency report omitted a scene".into());
    }
    let scenes = scenes
        .into_iter()
        .map(|(_, scene)| (scene.id().to_owned(), scene))
        .collect();

    let input = InputBindings::parse_json(&fs::read(root.join("input.json"))?)
        .map_err(|error| error.to_string())?;

    Ok(Content {
        manifest,
        scenes,
        animation,
        input,
        texture_bytes,
    })
}

fn find_content_root() -> Result<PathBuf, Box<dyn Error>> {
    let current = std::env::current_dir()?;
    for directory in current.ancestors() {
        let candidate = directory.join(CONTENT_ROOT);
        if candidate.is_dir() {
            return Ok(candidate);
        }
    }
    Err(format!("cannot find {CONTENT_ROOT} from {}", current.display()).into())
}

fn format_diagnostics(diagnostics: &[hycel_project::Diagnostic]) -> String {
    diagnostics
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn has_tag(entity: &hycel_project::SceneEntity, tag: &str) -> bool {
    entity.tags().iter().any(|candidate| candidate == tag)
}

fn scene_entity<'a>(
    scene: &'a SceneDocument,
    tag: &str,
) -> Result<&'a hycel_project::SceneEntity, Box<dyn Error>> {
    let mut matches = scene
        .entities()
        .iter()
        .filter(|entity| has_tag(entity, tag));
    let entity = matches
        .next()
        .ok_or_else(|| format!("scene {} has no {tag} entity", scene.name()))?;
    if matches.next().is_some() && tag != "platform" && tag != "hazard" {
        return Err(format!("scene {} has more than one {tag} entity", scene.name()).into());
    }
    Ok(entity)
}

#[derive(Clone, Debug)]
struct Checkpoint {
    entity_id: String,
    position: [i64; 2],
    half_extents: [i64; 2],
}

#[derive(Clone, Copy, Debug)]
struct ExitGate {
    position: [i64; 2],
    half_extents: [i64; 2],
}

#[derive(Clone, Copy, Debug)]
struct Collectible {
    position: [i64; 2],
    half_extents: [i64; 2],
}

struct Room {
    scene: SceneDocument,
    physics: PhysicsWorld,
    physics_tick: u64,
    player: BodyId,
    player_half_extents: [i64; 2],
    hazard_ids: BTreeSet<BodyId>,
    checkpoint: Checkpoint,
    exit: ExitGate,
    collectible: Collectible,
    collectible_collected: bool,
    respawn_position: [i64; 2],
    active_checkpoint_id: Option<String>,
    grounded: bool,
}

impl Room {
    fn new(scene: SceneDocument, saved_checkpoint: Option<&str>) -> Result<Self, Box<dyn Error>> {
        let config = PhysicsConfig::new(
            60,
            Vec2::new(SimScalar::ZERO, SimScalar::from_milli_units(GRAVITY_MILLI)),
        );
        let mut physics = PhysicsWorld::new(config)?;
        let player_entity = scene_entity(&scene, "player")?;
        let player_half_extents = player_entity.transform().scale_milli();
        validate_half_extents(player_half_extents)?;

        let checkpoint_entity = scene_entity(&scene, "checkpoint")?;
        let checkpoint = Checkpoint {
            entity_id: checkpoint_entity.id().to_owned(),
            position: checkpoint_entity.transform().translation_milli(),
            half_extents: positive_half_extents(checkpoint_entity.transform().scale_milli()),
        };
        let exit_entity = scene_entity(&scene, "exit")?;
        let exit = ExitGate {
            position: exit_entity.transform().translation_milli(),
            half_extents: positive_half_extents(exit_entity.transform().scale_milli()),
        };
        let collectible_entity = scene_entity(&scene, "collectible")?;
        let collectible = Collectible {
            position: collectible_entity.transform().translation_milli(),
            half_extents: positive_half_extents(collectible_entity.transform().scale_milli()),
        };

        let mut platform_count = 0_usize;
        let mut hazard_ids = BTreeSet::new();
        for entity in scene.entities() {
            let is_platform = has_tag(entity, "platform");
            let is_hazard = has_tag(entity, "hazard");
            if !is_platform && !is_hazard {
                continue;
            }
            let half_extents = entity.transform().scale_milli();
            validate_half_extents(half_extents)?;
            let body = physics.insert_box(BoxBody::new(
                BodyKind::Fixed,
                scene_vector(entity.transform().translation_milli()),
                scene_vector(half_extents),
            ))?;
            if is_platform {
                platform_count += 1;
            }
            if is_hazard {
                hazard_ids.insert(body);
            }
        }
        if platform_count == 0 || hazard_ids.is_empty() {
            return Err(format!(
                "scene {} needs at least one platform and hazard",
                scene.name()
            )
            .into());
        }

        let initial_position = player_entity.transform().translation_milli();
        let (spawn_position, active_checkpoint_id) = match saved_checkpoint {
            Some(id) if id == checkpoint.entity_id => (checkpoint.position, Some(id.to_owned())),
            Some(_) => {
                return Err(
                    format!("saved checkpoint is not present in scene {}", scene.name()).into(),
                );
            }
            None => (initial_position, None),
        };
        let player = physics.insert_box(
            BoxBody::new(
                BodyKind::Dynamic,
                scene_vector(spawn_position),
                scene_vector(player_half_extents),
            )
            .with_continuous_collision_detection(),
        )?;

        Ok(Self {
            scene,
            physics,
            physics_tick: 0,
            player,
            player_half_extents,
            hazard_ids,
            checkpoint,
            exit,
            collectible,
            collectible_collected: false,
            respawn_position: spawn_position,
            active_checkpoint_id,
            grounded: false,
        })
    }

    fn reset_player(&mut self) -> Result<(), Box<dyn Error>> {
        self.physics.remove_body(self.player)?;
        self.player = self.physics.insert_box(
            BoxBody::new(
                BodyKind::Dynamic,
                scene_vector(self.respawn_position),
                scene_vector(self.player_half_extents),
            )
            .with_continuous_collision_detection(),
        )?;
        self.grounded = false;
        Ok(())
    }

    fn player_position(&self) -> Result<[i64; 2], Box<dyn Error>> {
        let position = self.physics.body_state(self.player)?.position;
        Ok([position.x.milli_units(), position.y.milli_units()])
    }

    fn has_ground_support(&self, position: [i64; 2]) -> bool {
        const SUPPORT_TOLERANCE_MILLI: u64 = 40;
        let player_bottom = position[1].saturating_add(self.player_half_extents[1]);
        self.scene
            .entities()
            .iter()
            .filter(|entity| has_tag(entity, "platform"))
            .any(|entity| {
                let center = entity.transform().translation_milli();
                let half = entity.transform().scale_milli();
                let horizontal_range =
                    u64::try_from(self.player_half_extents[0] + half[0]).unwrap_or(u64::MAX);
                let platform_top = center[1].saturating_sub(half[1]);
                position[0].abs_diff(center[0]) <= horizontal_range
                    && player_bottom.abs_diff(platform_top) <= SUPPORT_TOLERANCE_MILLI
            })
    }

    fn tick(&mut self, horizontal: i16, jump_pressed: bool) -> Result<RoomTick, Box<dyn Error>> {
        let current = self.physics.body_state(self.player)?;
        let horizontal_velocity = i64::from(horizontal) * PLAYER_SPEED_MILLI / i64::from(i16::MAX);
        let jumped = jump_pressed && self.grounded;
        let vertical_velocity = if jumped {
            JUMP_SPEED_MILLI
        } else {
            current.velocity.y.milli_units()
        };
        self.physics.set_velocity(
            self.player,
            Vec2::new(
                SimScalar::from_milli_units(horizontal_velocity),
                SimScalar::from_milli_units(vertical_velocity),
            ),
        )?;

        let player = self.player;
        let hazards = &self.hazard_ids;
        let events = self.physics.step(self.physics_tick)?.to_vec();
        self.physics_tick = self
            .physics_tick
            .checked_add(1)
            .ok_or("room tick exhausted")?;
        let mut hazard_contact = false;
        for event in events {
            let pair = event.pair;
            let touches_player = pair.first == player || pair.second == player;
            if !touches_player {
                continue;
            }
            let other = if pair.first == player {
                pair.second
            } else {
                pair.first
            };
            match event.kind {
                hycel_physics::ContactKind::Started if hazards.contains(&other) => {
                    hazard_contact = true;
                }
                _ => {}
            }
        }
        let position = self.player_position()?;
        self.grounded = self.has_ground_support(position);
        Ok(RoomTick {
            hazard_contact,
            jumped,
            position,
        })
    }
}

fn validate_half_extents(half_extents: [i64; 2]) -> Result<(), Box<dyn Error>> {
    if half_extents[0] <= 0 || half_extents[1] <= 0 {
        return Err("scene physics sizes must be positive half-extents".into());
    }
    Ok(())
}

fn positive_half_extents(mut half_extents: [i64; 2]) -> [i64; 2] {
    half_extents[0] = half_extents[0].max(1);
    half_extents[1] = half_extents[1].max(1);
    half_extents
}

fn scene_vector(values: [i64; 2]) -> Vec2 {
    Vec2::new(
        SimScalar::from_milli_units(values[0]),
        SimScalar::from_milli_units(values[1]),
    )
}

#[derive(Clone, Copy, Debug, Default)]
struct RoomTick {
    hazard_contact: bool,
    jumped: bool,
    position: [i64; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GameEvent {
    Jumped,
    CheckpointReached,
    Died,
    SceneChanged,
    Completed,
    CollectibleCollected,
}

#[derive(Debug, Default)]
struct TickEffects {
    events: Vec<GameEvent>,
    progress_changed: bool,
}

impl TickEffects {
    fn has(&self, event: GameEvent) -> bool {
        self.events.contains(&event)
    }
}

struct Game {
    scenes: Vec<(String, SceneDocument)>,
    room: Room,
    animation_clip: AnimationClipDocument,
    animation: AnimationPlayer,
    transitions: SceneTransitionQueue,
    progress: GameProgress,
    pending_transition: bool,
    completed: bool,
}

impl Game {
    fn new(content: &Content, progress: GameProgress) -> Result<Self, Box<dyn Error>> {
        let scene_index = content
            .scenes
            .iter()
            .position(|(_, scene)| scene.id() == progress.current_scene_id())
            .ok_or("save references a scene that is not in this game")?;
        let scene = content.scenes[scene_index].1.clone();
        let room = Room::new(scene.clone(), progress.checkpoint_entity_id())?;
        let transitions = SceneTransitionQueue::new(scene.id().to_owned());
        Ok(Self {
            scenes: content.scenes.clone(),
            room,
            animation_clip: content.animation.clone(),
            animation: AnimationPlayer::new(content.animation.clone()),
            transitions,
            completed: progress
                .completed_scene_ids()
                .iter()
                .any(|completed_id| completed_id == scene.id())
                && scene_index + 1 == content.scenes.len(),
            progress,
            pending_transition: false,
        })
    }

    #[allow(clippy::too_many_lines)] // One fixed tick coordinates state, triggers, and deferred transitions.
    fn tick(
        &mut self,
        tick: u64,
        horizontal: i16,
        jump_pressed: bool,
        restart_pressed: bool,
    ) -> Result<TickEffects, Box<dyn Error>> {
        let mut effects = TickEffects::default();
        if let Some(scene) = self.transitions.apply(tick, &self.scenes)? {
            self.room = Room::new(scene.clone(), None)?;
            self.progress = GameProgress::new(
                scene.id(),
                None,
                self.progress.completed_scene_ids().to_vec(),
            )?;
            self.pending_transition = false;
            self.animation = AnimationPlayer::new(self.animation_clip.clone());
            effects.events.push(GameEvent::SceneChanged);
            effects.progress_changed = true;
        }
        if self.completed {
            if restart_pressed {
                let first_scene = self.scenes.first().ok_or("game has no scenes")?.1.clone();
                self.room = Room::new(first_scene.clone(), None)?;
                self.transitions = SceneTransitionQueue::new(first_scene.id().to_owned());
                self.progress = GameProgress::new(first_scene.id(), None, Vec::new())?;
                self.completed = false;
                self.animation = AnimationPlayer::new(self.animation_clip.clone());
                effects.progress_changed = true;
                effects.events.push(GameEvent::SceneChanged);
            }
            self.animation.advance(tick)?;
            return Ok(effects);
        }
        if restart_pressed {
            self.room.reset_player()?;
        }
        let room_tick = self.room.tick(horizontal, jump_pressed)?;
        if room_tick.jumped {
            effects.events.push(GameEvent::Jumped);
        }
        if room_tick.hazard_contact {
            self.room.reset_player()?;
            effects.events.push(GameEvent::Died);
        } else {
            let checkpoint = self.room.checkpoint.clone();
            if !self.room.collectible_collected
                && overlaps(
                    room_tick.position,
                    self.room.player_half_extents,
                    self.room.collectible.position,
                    self.room.collectible.half_extents,
                )
            {
                self.room.collectible_collected = true;
                effects.events.push(GameEvent::CollectibleCollected);
            }
            if self.progress.checkpoint_entity_id() != Some(checkpoint.entity_id.as_str())
                && overlaps(
                    room_tick.position,
                    self.room.player_half_extents,
                    checkpoint.position,
                    checkpoint.half_extents,
                )
            {
                self.room.respawn_position = checkpoint.position;
                self.room.active_checkpoint_id = Some(checkpoint.entity_id.clone());
                self.progress = GameProgress::new(
                    self.room.scene.id(),
                    Some(checkpoint.entity_id),
                    self.progress.completed_scene_ids().to_vec(),
                )?;
                effects.events.push(GameEvent::CheckpointReached);
                effects.progress_changed = true;
            }

            if !self.pending_transition
                && overlaps(
                    room_tick.position,
                    self.room.player_half_extents,
                    self.room.exit.position,
                    self.room.exit.half_extents,
                )
            {
                let current_index = self
                    .scenes
                    .iter()
                    .position(|(_, scene)| scene.id() == self.room.scene.id())
                    .ok_or("current scene is missing")?;
                let mut completed = self
                    .progress
                    .completed_scene_ids()
                    .iter()
                    .cloned()
                    .collect::<BTreeSet<_>>();
                completed.insert(self.room.scene.id().to_owned());
                if let Some((_, next_scene)) = self.scenes.get(current_index + 1) {
                    self.progress = GameProgress::new(
                        self.room.scene.id(),
                        self.progress.checkpoint_entity_id().map(str::to_owned),
                        completed.into_iter().collect(),
                    )?;
                    self.transitions.request(next_scene.id().to_owned(), tick)?;
                    self.pending_transition = true;
                    effects.progress_changed = true;
                } else if !self.completed {
                    self.progress = GameProgress::new(
                        self.room.scene.id(),
                        self.progress.checkpoint_entity_id().map(str::to_owned),
                        completed.into_iter().collect(),
                    )?;
                    self.completed = true;
                    effects.events.push(GameEvent::Completed);
                    effects.progress_changed = true;
                }
            }
        }
        self.animation.advance(tick)?;
        Ok(effects)
    }
}

fn overlaps(a: [i64; 2], a_half: [i64; 2], b: [i64; 2], b_half: [i64; 2]) -> bool {
    a[0].abs_diff(b[0]) <= u64::try_from(a_half[0] + b_half[0]).unwrap_or(u64::MAX)
        && a[1].abs_diff(b[1]) <= u64::try_from(a_half[1] + b_half[1]).unwrap_or(u64::MAX)
}

fn initial_progress(content: &Content) -> Result<GameProgress, Box<dyn Error>> {
    let first_scene = content.scenes.first().ok_or("game has no scenes")?.1.id();
    Ok(GameProgress::new(first_scene, None, Vec::new())?)
}

type OpenProgress = (Option<ProgressStore>, GameProgress, Option<String>);

fn open_progress_store(
    content: &Content,
    recover_requested: bool,
) -> Result<OpenProgress, Box<dyn Error>> {
    let store = match ProgressStore::for_game(content.manifest.project_id()) {
        Ok(store) => store,
        Err(error) => {
            return Ok((
                None,
                initial_progress(content)?,
                Some(format!("Progress is memory-only: {error}")),
            ));
        }
    };
    match store.load() {
        Ok(progress) => Ok((Some(store), progress, None)),
        Err(SaveError::NotFound) if store.backup_path().exists() => {
            if !recover_requested {
                return Err(format!("a backup save exists at {}; relaunch with --recover-save to restore it", store.backup_path().display()).into());
            }
            let progress = store.recover_backup()?;
            Ok((Some(store), progress, Some("Restored the explicitly requested backup save".to_owned())))
        }
        Err(SaveError::NotFound) => {
            let progress = initial_progress(content)?;
            let warning = store.save(&progress).err().map(|error| format!("Progress save failed: {error}"));
            Ok((Some(store), progress, warning))
        }
        Err(error) if recover_requested => {
            let progress = store.recover_backup()?;
            eprintln!("Recovered after {error}");
            Ok((Some(store), progress, Some("Restored the explicitly requested backup save".to_owned())))
        }
        Err(error) => Err(format!("cannot load progress safely: {error}; use --recover-save only if you intend to restore the last-known-good backup").into()),
    }
}

struct GameHost {
    content: Content,
    game: Game,
    store: Option<ProgressStore>,
    save_warning: Option<String>,
    mapper: InputMapper,
    bindings: InputBindings,
    action_events: ActionEventTracker,
    clock: FixedClock,
    next_tick: u64,
    previous_frame: Instant,
    renderer: Option<Renderer>,
    textures: BTreeMap<String, TextureId>,
    audio: AudioService,
    jump_sound: AudioClip,
    audio_note: String,
    started: bool,
    window: Option<WindowHandle>,
}

impl GameHost {
    fn new(recover_requested: bool) -> Result<Self, Box<dyn Error>> {
        let content = load_content()?;
        let (store, progress, save_warning) = open_progress_store(&content, recover_requested)?;
        let game = Game::new(&content, progress)?;
        let bindings = content.input.clone();
        let mapper = InputMapper::new(bindings.clone());
        let mut audio = AudioService::open();
        let audio_note = match audio.status() {
            AudioStatus::Active => "Audio: available (Space jumps)".to_owned(),
            AudioStatus::Unavailable { reason } => {
                format!("Audio: silent fallback ({})", trim_text(&reason, 48))
            }
        };
        let jump_sound = AudioClip::parse(&make_jump_wav())?;
        let music_sound = AudioClip::parse(&make_music_wav())?;
        let _ = audio.play_music(&music_sound);
        Ok(Self {
            content,
            game,
            store,
            save_warning,
            mapper,
            bindings,
            action_events: ActionEventTracker::new(),
            clock: FixedClock::new(60)?,
            next_tick: 0,
            previous_frame: Instant::now(),
            renderer: None,
            textures: BTreeMap::new(),
            audio,
            jump_sound,
            audio_note,
            started: false,
            window: None,
        })
    }

    fn handle(&mut self, event: PlatformEvent) -> Result<EventAction, Box<dyn Error>> {
        match event {
            PlatformEvent::WindowCreated { window, metrics: _ } => {
                let mut renderer = Renderer::new(Arc::new(window.clone()))?;
                for (resource_id, bytes) in &self.content.texture_bytes {
                    let image = RgbaImage::new(8, 8, bytes.clone())?;
                    self.textures
                        .insert(resource_id.clone(), renderer.load_texture(image)?);
                }
                self.textures.insert(
                    "platform".to_owned(),
                    renderer.load_texture(solid_image([61, 90, 128, 255]))?,
                );
                self.textures.insert(
                    "hazard".to_owned(),
                    renderer.load_texture(solid_image([214, 69, 93, 255]))?,
                );
                self.textures.insert(
                    "checkpoint".to_owned(),
                    renderer.load_texture(solid_image([237, 174, 73, 255]))?,
                );
                self.textures.insert(
                    "exit".to_owned(),
                    renderer.load_texture(solid_image([42, 157, 143, 255]))?,
                );
                self.textures.insert(
                    "collectible".to_owned(),
                    renderer.load_texture(solid_image([76, 201, 240, 255]))?,
                );
                self.renderer = Some(renderer);
                self.window = Some(window.clone());
                window.request_redraw();
                Ok(EventAction::Continue)
            }
            PlatformEvent::Input(input) => {
                self.mapper.handle_event(input);
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
                Ok(EventAction::Continue)
            }
            PlatformEvent::FocusChanged(focused) => {
                self.mapper.focus_changed(focused);
                Ok(EventAction::Continue)
            }
            PlatformEvent::Resized(size) => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(SurfaceSize {
                        width: size.width,
                        height: size.height,
                    })?;
                }
                Ok(EventAction::Continue)
            }
            PlatformEvent::RedrawRequested => {
                self.advance_frame()?;
                self.render_frame()?;
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
                Ok(EventAction::Continue)
            }
            PlatformEvent::CloseRequested => Ok(EventAction::Exit),
            PlatformEvent::ScaleFactorChanged(_) => Ok(EventAction::Continue),
        }
    }

    fn advance_frame(&mut self) -> Result<(), Box<dyn Error>> {
        let now = Instant::now();
        let elapsed = now.duration_since(self.previous_frame);
        self.previous_frame = now;
        let elapsed_nanos = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
        let FrameAdvance { steps, .. } =
            self.clock
                .advance_frame(elapsed_nanos, TimeScale::NORMAL, 8)?;
        for _ in 0..steps {
            let tick = self.next_tick;
            let frame = self.mapper.frame(tick);
            let events = self
                .action_events
                .events_for_frame(&frame, &self.bindings)?;
            let jump_pressed = events.iter().any(|event| {
                event.action_id() == ACTION_JUMP && event.kind() == ButtonActionEventKind::Pressed
            });
            let restart_pressed = events.iter().any(|event| {
                event.action_id() == ACTION_RESTART
                    && event.kind() == ButtonActionEventKind::Pressed
            });
            if !self.started {
                if jump_pressed || restart_pressed {
                    self.started = true;
                    println!("Platformer run started");
                } else {
                    self.next_tick = self
                        .next_tick
                        .checked_add(1)
                        .ok_or("simulation tick exhausted")?;
                    continue;
                }
            }
            let effects = self.game.tick(
                tick,
                frame.axis(ACTION_MOVE_X),
                jump_pressed,
                restart_pressed,
            )?;
            self.next_tick = self
                .next_tick
                .checked_add(1)
                .ok_or("simulation tick exhausted")?;
            if effects.progress_changed {
                self.persist_progress();
            }
            if (effects.has(GameEvent::Jumped)
                || effects.has(GameEvent::CheckpointReached)
                || effects.has(GameEvent::CollectibleCollected)
                || effects.has(GameEvent::Completed))
                && self.audio.play_effect(&self.jump_sound) == PlaybackOutcome::SkippedError
            {
                eprintln!("Gameplay sound could not be decoded or played");
            }
            if effects.has(GameEvent::CheckpointReached) {
                println!(
                    "Checkpoint reached: {}",
                    self.game.room.checkpoint.entity_id
                );
            }
            if effects.has(GameEvent::CollectibleCollected) {
                println!("Collected a shard");
            }
            if effects.has(GameEvent::Died) {
                println!("Hazard hit; respawned at the active checkpoint");
            }
            if effects.has(GameEvent::SceneChanged) {
                println!("Now entering {}", self.game.room.scene.name());
            }
            if effects.has(GameEvent::Completed) {
                println!("Vertical slice complete!");
            }
        }
        for error in self.audio.poll_errors() {
            eprintln!("Non-fatal audio diagnostic: {error}");
        }
        Ok(())
    }

    fn persist_progress(&mut self) {
        if let Some(store) = &self.store {
            match store.save(&self.game.progress) {
                Ok(()) => self.save_warning = None,
                Err(error) => {
                    let message = format!("Progress save failed: {error}");
                    eprintln!("{message}");
                    self.save_warning = Some(message);
                }
            }
        }
    }

    fn render_frame(&mut self) -> Result<(), Box<dyn Error>> {
        let Some(renderer) = &mut self.renderer else {
            return Ok(());
        };
        let scene = &self.game.room.scene;
        let mut sprites = Vec::new();
        for entity in scene.entities() {
            let (tag, layer) = if has_tag(entity, "platform") {
                ("platform", 0)
            } else if has_tag(entity, "hazard") {
                ("hazard", 1)
            } else if has_tag(entity, "checkpoint") {
                ("checkpoint", 2)
            } else if has_tag(entity, "exit") {
                ("exit", 3)
            } else if has_tag(entity, "collectible") && !self.game.room.collectible_collected {
                ("collectible", 3)
            } else {
                continue;
            };
            let center = visual_pair(entity.transform().translation_milli())?;
            let half = visual_pair(entity.transform().scale_milli())?;
            let texture = *self.textures.get(tag).ok_or("sample texture missing")?;
            sprites.push(Sprite {
                layer,
                ..Sprite::new(texture, center, [half[0] * 2.0, half[1] * 2.0])
            });
        }
        let position = self.game.room.player_position()?;
        let center = visual_pair(position)?;
        let half = visual_pair(self.game.room.player_half_extents)?;
        let frame_resource = self.game.animation.texture_id();
        let player_texture = *self
            .textures
            .get(frame_resource)
            .ok_or("animation frame texture is not loaded")?;
        sprites.push(Sprite {
            layer: 4,
            ..Sprite::new(player_texture, center, [half[0] * 2.0, half[1] * 2.0])
        });

        let camera = Camera2D {
            center: [center[0], 0.38],
            zoom: 2.5,
        };
        let mut lines = vec![
            format!(
                "{}  |  A/D or arrows: move   Space: jump   R: checkpoint",
                scene.name()
            ),
            self.audio_note.clone(),
        ];
        if !self.started {
            lines.push("Collect the shards, avoid hazards, and reach the exit.".to_owned());
            lines.push("Press Space to start.".to_owned());
        } else if self.game.completed {
            lines.push("You made it! Press R to play again.".to_owned());
        }
        if let Some(warning) = &self.save_warning {
            lines.push(format!("Progress warning: {}", trim_text(warning, 120)));
        }
        let texts = lines
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let index = u16::try_from(index).unwrap_or(u16::MAX);
                DebugText::new([14, 22 + u32::from(index) * 22], text.clone())
            })
            .collect::<Vec<_>>();
        match renderer.render_scene(camera, &sprites, &texts)? {
            FrameOutcome::Presented | FrameOutcome::Skipped | FrameOutcome::Reconfigured => {}
        }
        Ok(())
    }
}

fn visual_pair(values: [i64; 2]) -> Result<[f32; 2], Box<dyn Error>> {
    let x = i16::try_from(values[0])?;
    let y = i16::try_from(values[1])?;
    Ok([f32::from(x) / 1_000.0, f32::from(y) / 1_000.0])
}

fn trim_text(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let prefix = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

fn solid_image(color: [u8; 4]) -> RgbaImage {
    let pixels = color.repeat(8 * 8);
    RgbaImage::new(8, 8, pixels).expect("solid 8x8 RGBA texture is valid")
}

fn make_music_wav() -> Vec<u8> {
    let sample_count = WAVE_SAMPLE_RATE * 2;
    let data_bytes = sample_count * 2;
    let mut wav = Vec::with_capacity(usize::try_from(44 + data_bytes).unwrap_or(44));
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&WAVE_SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(WAVE_SAMPLE_RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_bytes.to_le_bytes());
    let notes = [220_u32, 277, 330, 277];
    for sample in 0..sample_count {
        let note_index = usize::try_from((sample / (WAVE_SAMPLE_RATE / 2)) % 4).unwrap_or(0);
        let frequency = notes[note_index];
        let phase = (sample * frequency / WAVE_SAMPLE_RATE) % 2;
        let amplitude = if phase == 0 { 500_i16 } else { -500_i16 };
        wav.extend_from_slice(&amplitude.to_le_bytes());
    }
    wav
}

fn make_jump_wav() -> Vec<u8> {
    let sample_count = 1_102_u32;
    let data_bytes = sample_count * 2;
    let mut wav = Vec::with_capacity((44 + data_bytes) as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&WAVE_SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(WAVE_SAMPLE_RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_bytes.to_le_bytes());
    for sample in 0..sample_count {
        let envelope = i32::try_from((sample_count - sample) * 9_000 / sample_count).unwrap_or(0);
        let phase = (sample * 440 / WAVE_SAMPLE_RATE) % 2;
        let amplitude = if phase == 0 { envelope } else { -envelope };
        wav.extend_from_slice(&i16::try_from(amplitude).unwrap_or_default().to_le_bytes());
    }
    wav
}

fn main() -> Result<(), Box<dyn Error>> {
    let recover_requested = std::env::args().any(|argument| argument == "--recover-save");
    let mut host = GameHost::new(recover_requested)?;
    if let Some(warning) = &host.save_warning {
        eprintln!("{warning}");
    }
    let failure = Rc::new(RefCell::new(None::<String>));
    let callback_failure = Rc::clone(&failure);
    let config = WindowConfig::new("Hycel Platformer — Vertical Slice", 960, 540)?;
    run_window(config, move |event| match host.handle(event) {
        Ok(action) => action,
        Err(error) => {
            *callback_failure.borrow_mut() = Some(error.to_string());
            EventAction::Exit
        }
    })?;
    if let Some(error) = failure.borrow_mut().take() {
        return Err(error.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Game, initial_progress, load_content, make_jump_wav, make_music_wav};
    use hycel_audio::AudioClip;

    #[test]
    fn authored_vertical_slice_content_is_strict_and_connected() {
        let content = load_content().expect("sample project content should validate");
        assert_eq!(content.scenes.len(), 2);
        assert_eq!(content.animation.frames().len(), 2);
        assert_eq!(content.input.axes().len(), 1);
        assert_eq!(content.texture_bytes.len(), 2);
    }

    #[test]
    fn fixed_tick_controls_move_and_jump_without_a_window() {
        let content = load_content().unwrap();
        let progress = initial_progress(&content).unwrap();
        let mut game = Game::new(&content, progress).unwrap();
        let mut saw_jump = false;
        let mut saw_collectible = false;
        let mut saw_checkpoint = false;
        let mut saw_transition = false;
        let mut saw_completion = false;
        for tick in 0..120 {
            let jump = tick == 10 || tick == 55 || tick == 95;
            let effects = game.tick(tick, i16::MAX, jump, false).unwrap();
            saw_jump |= effects.has(super::GameEvent::Jumped);
            saw_collectible |= effects.has(super::GameEvent::CollectibleCollected);
            saw_checkpoint |= effects.has(super::GameEvent::CheckpointReached);
            saw_transition |= effects.has(super::GameEvent::SceneChanged);
            saw_completion |= effects.has(super::GameEvent::Completed);
        }
        let position = game.room.player_position().unwrap();
        assert!(
            position[0] > 0,
            "player should move right under fixed-tick input"
        );
        assert!(
            saw_jump,
            "the fixed-tick jump edge should launch the grounded player"
        );
        assert!(
            saw_collectible,
            "touching a shard should emit a gameplay event"
        );
        assert!(
            saw_checkpoint,
            "touching a checkpoint should update progress"
        );
        assert!(
            saw_transition,
            "reaching the first exit should load the next room"
        );
        assert!(
            saw_completion,
            "reaching the second exit should complete the slice"
        );
        assert_eq!(game.room.scene.id(), content.scenes[1].1.id());
        assert!(
            game.progress
                .completed_scene_ids()
                .contains(&content.scenes[1].1.id().to_owned())
        );
        assert!(game.completed);
        let restored = Game::new(&content, game.progress.clone()).unwrap();
        assert!(
            restored.completed,
            "completion should survive process restarts"
        );
        assert!(!game.animation.texture_id().is_empty());
    }

    #[test]
    fn only_top_side_platform_contacts_enable_jumping() {
        let content = load_content().unwrap();
        let room = super::Room::new(content.scenes[1].1.clone(), None).unwrap();
        assert!(
            room.has_ground_support([40, 400]),
            "player above the raised platform should be supported"
        );
        assert!(
            !room.has_ground_support([40, 600]),
            "touching the platform underside must not enable a jump"
        );
    }

    #[test]
    fn hazard_contact_respawns_at_the_active_checkpoint() {
        let content = load_content().unwrap();
        let progress = initial_progress(&content).unwrap();
        let mut game = Game::new(&content, progress).unwrap();
        let mut died = false;
        for tick in 0..40 {
            let effects = game.tick(tick, i16::MAX, false, false).unwrap();
            died |= effects.has(super::GameEvent::Died);
        }
        assert!(died, "walking into the hazard should trigger a respawn");
        assert_eq!(
            game.room.active_checkpoint_id.as_deref(),
            game.progress.checkpoint_entity_id()
        );
    }

    #[test]
    fn generated_effect_is_a_bounded_decodable_wav() {
        let clip = AudioClip::parse(&make_jump_wav()).unwrap();
        assert!(clip.encoded_len() > 0);
        let music = AudioClip::parse(&make_music_wav()).unwrap();
        assert!(music.encoded_len() > clip.encoded_len());
    }
}
