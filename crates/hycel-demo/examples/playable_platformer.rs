//! Interactive compiled-in reference game showing Hycel runtime APIs working together.
//!
//! Run from the workspace root with `cargo run -p hycel-demo --example playable_platformer`.
//! The default authored project is `examples/bellglass-courier`; the earlier two-room
//! platformer fixture remains supported for compatibility and regression coverage.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use hycel_animation::{AnimationPlayer, SceneTransitionQueue};
use hycel_assets::{AssetDependencyReport, hash_source_file};
use hycel_audio::{AudioClip, AudioService, AudioStatus, PlaybackOutcome};
use hycel_core::{
    CanonicalState, CanonicalWriter, FixedClock, FrameAdvance, Replay, SimScalar, TimeScale, Vec2,
};
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

const CONTENT_ROOT: &str = "examples/bellglass-courier";
const MAX_SOURCE_BYTES: u64 = 64 * 1024;
const ACTION_MOVE_X: u16 = 0;
const ACTION_JUMP: u16 = 1;
const ACTION_RESTART: u16 = 2;
const ACTION_ECHO: u16 = 3;
const ECHO_HISTORY_TICKS: usize = 120;
const GUST_SPEED_MILLI: i64 = 100;
const FALL_OUT_Y_MILLI: i64 = 1_500;
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

fn load_content(root: &std::path::Path) -> Result<Content, Box<dyn Error>> {
    let manifest_text = fs::read_to_string(root.join("hycel.toml"))?;
    let manifest = ProjectManifest::parse_toml(&manifest_text)?;

    let mut components = ComponentRegistry::default();
    components.register("hycel.animation", 1, false, ["clip_id"])?;
    components.mark_resource_reference("hycel.animation", "clip_id")?;

    let mut scenes = Vec::new();
    for file in [
        "scenes/first-room.json",
        "scenes/middle-room.json",
        "scenes/last-room.json",
    ] {
        let path = root.join(file);
        if file == "scenes/middle-room.json" && !path.exists() {
            continue;
        }
        let bytes = fs::read(path)?;
        let scene = SceneDocument::parse_json_with_registry(&bytes, file, &components)
            .map_err(|diagnostics| format_diagnostics(&diagnostics))?;
        scenes.push((file.to_owned(), scene));
    }
    if scenes.len() < 2 {
        return Err("reference game requires at least two authored scenes".into());
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
            let source_hash = hash_source_file(root, descriptor.source(), MAX_SOURCE_BYTES)?;
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

#[derive(Clone, Debug)]
struct Collectible {
    entity_id: String,
    position: [i64; 2],
    half_extents: [i64; 2],
}

#[derive(Clone, Copy, Debug)]
struct PressurePlate {
    position: [i64; 2],
    half_extents: [i64; 2],
}

#[derive(Clone, Copy, Debug)]
struct GustZone {
    position: [i64; 2],
    half_extents: [i64; 2],
}

impl GustZone {
    fn position_at(self, tick: u64) -> [i64; 2] {
        let phase = i64::try_from(tick % 120).unwrap_or(0);
        let horizontal_sweep = if phase < 60 {
            -300 + phase * 10
        } else {
            300 - (phase - 60) * 10
        };
        [
            self.position[0].saturating_add(horizontal_sweep),
            self.position[1],
        ]
    }
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
    pressure_plates: Vec<PressurePlate>,
    gust: Option<GustZone>,
    gate_open: bool,
    respawn_position: [i64; 2],
    active_checkpoint_id: Option<String>,
    grounded: bool,
}

impl Room {
    #[allow(clippy::too_many_lines)] // One authored scene is converted into bounded collision/trigger state.
    fn new(
        scene: SceneDocument,
        saved_checkpoint: Option<&str>,
        collected_item_ids: &[String],
    ) -> Result<Self, Box<dyn Error>> {
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
            entity_id: collectible_entity.id().to_owned(),
            position: collectible_entity.transform().translation_milli(),
            half_extents: positive_half_extents(collectible_entity.transform().scale_milli()),
        };
        let collectible_collected = collected_item_ids
            .iter()
            .any(|item_id| item_id == &collectible.entity_id);
        let pressure_plates = scene
            .entities()
            .iter()
            .filter(|entity| has_tag(entity, "plate"))
            .map(|entity| PressurePlate {
                position: entity.transform().translation_milli(),
                half_extents: positive_half_extents(entity.transform().scale_milli()),
            })
            .collect::<Vec<_>>();
        if pressure_plates.len() > 8 {
            return Err(format!(
                "scene {} exceeds the eight-plate reference-game limit",
                scene.name()
            )
            .into());
        }
        let gust = scene
            .entities()
            .iter()
            .find(|entity| has_tag(entity, "gust"))
            .map(|entity| GustZone {
                position: entity.transform().translation_milli(),
                half_extents: positive_half_extents(entity.transform().scale_milli()),
            });

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
            collectible_collected,
            pressure_plates,
            gust,
            gate_open: false,
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

    fn all_pressure_plates_held(
        &self,
        player_position: [i64; 2],
        echo_position: Option<[i64; 2]>,
    ) -> bool {
        self.pressure_plates.is_empty()
            || self.pressure_plates.iter().all(|plate| {
                let player_holds = overlaps(
                    player_position,
                    self.player_half_extents,
                    plate.position,
                    plate.half_extents,
                );
                let echo_holds = echo_position.is_some_and(|position| {
                    overlaps(
                        position,
                        self.player_half_extents,
                        plate.position,
                        plate.half_extents,
                    )
                });
                player_holds || echo_holds
            })
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
        let in_gust = self.gust.is_some_and(|gust| {
            overlaps(
                [
                    current.position.x.milli_units(),
                    current.position.y.milli_units(),
                ],
                self.player_half_extents,
                gust.position_at(self.physics_tick),
                gust.half_extents,
            )
        });
        let horizontal_velocity = i64::from(horizontal) * PLAYER_SPEED_MILLI / i64::from(i16::MAX)
            + if in_gust { GUST_SPEED_MILLI } else { 0 };
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
        hazard_contact |= position[1] > FALL_OUT_Y_MILLI;
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
    EchoLaunched,
    GateOpened,
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

struct EchoPlayback {
    positions: Vec<[i64; 2]>,
    next_position: usize,
}

struct Game {
    scenes: Vec<(String, SceneDocument)>,
    input_history: VecDeque<[i64; 2]>,
    echo: Option<EchoPlayback>,
    echo_position: Option<[i64; 2]>,
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
        let scene_ids = content
            .scenes
            .iter()
            .map(|(_, scene)| scene.id())
            .collect::<BTreeSet<_>>();
        if progress
            .completed_scene_ids()
            .iter()
            .any(|scene_id| !scene_ids.contains(scene_id.as_str()))
        {
            return Err("save references a completed scene that is not in this game".into());
        }
        let collectible_ids = content
            .scenes
            .iter()
            .flat_map(|(_, scene)| scene.entities())
            .filter(|entity| has_tag(entity, "collectible"))
            .map(hycel_project::SceneEntity::id)
            .collect::<BTreeSet<_>>();
        if progress
            .collected_item_ids()
            .iter()
            .any(|item_id| !collectible_ids.contains(item_id.as_str()))
        {
            return Err("save references a collected item that is not in this game".into());
        }
        let scene_index = content
            .scenes
            .iter()
            .position(|(_, scene)| scene.id() == progress.current_scene_id())
            .ok_or("save references a scene that is not in this game")?;
        let scene = content.scenes[scene_index].1.clone();
        let room = Room::new(
            scene.clone(),
            progress.checkpoint_entity_id(),
            progress.collected_item_ids(),
        )?;
        let transitions = SceneTransitionQueue::new(scene.id().to_owned());
        Ok(Self {
            scenes: content.scenes.clone(),
            input_history: VecDeque::with_capacity(ECHO_HISTORY_TICKS),
            echo: None,
            echo_position: None,
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
        echo_pressed: bool,
    ) -> Result<TickEffects, Box<dyn Error>> {
        let mut effects = TickEffects::default();
        if let Some(scene) = self.transitions.apply(tick, &self.scenes)? {
            self.input_history.clear();
            self.echo = None;
            self.echo_position = None;
            self.room = Room::new(scene.clone(), None, self.progress.collected_item_ids())?;
            self.progress = progress_with_items(
                scene.id(),
                None,
                self.progress.completed_scene_ids().to_vec(),
                self.progress.collected_item_ids().to_vec(),
            )?;
            self.pending_transition = false;
            self.animation = AnimationPlayer::new(self.animation_clip.clone());
            effects.events.push(GameEvent::SceneChanged);
            effects.progress_changed = true;
        }
        if self.completed {
            if restart_pressed {
                let first_scene = self.scenes.first().ok_or("game has no scenes")?.1.clone();
                self.room = Room::new(first_scene.clone(), None, &[])?;
                self.input_history.clear();
                self.echo = None;
                self.echo_position = None;
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
            self.input_history.clear();
            self.echo = None;
            self.echo_position = None;
        }
        if echo_pressed && !self.input_history.is_empty() {
            self.echo = Some(EchoPlayback {
                positions: self.input_history.iter().copied().collect(),
                next_position: 0,
            });
            effects.events.push(GameEvent::EchoLaunched);
        }
        self.echo_position = self.echo.as_mut().and_then(|echo| {
            let position = echo.positions.get(echo.next_position).copied();
            echo.next_position = echo.next_position.saturating_add(1);
            position
        });
        if self
            .echo
            .as_ref()
            .is_some_and(|echo| echo.next_position >= echo.positions.len())
        {
            self.echo = None;
        }
        let room_tick = self.room.tick(horizontal, jump_pressed)?;
        if room_tick.jumped {
            effects.events.push(GameEvent::Jumped);
        }
        if room_tick.hazard_contact {
            self.room.reset_player()?;
            self.input_history.clear();
            self.echo = None;
            self.echo_position = None;
            self.room.gate_open = false;
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
                let mut collected_item_ids = self.progress.collected_item_ids().to_vec();
                collected_item_ids.push(self.room.collectible.entity_id.clone());
                self.progress = progress_with_items(
                    self.room.scene.id(),
                    self.progress.checkpoint_entity_id().map(str::to_owned),
                    self.progress.completed_scene_ids().to_vec(),
                    collected_item_ids,
                )?;
                effects.events.push(GameEvent::CollectibleCollected);
                effects.progress_changed = true;
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
                self.progress = progress_with_items(
                    self.room.scene.id(),
                    Some(checkpoint.entity_id),
                    self.progress.completed_scene_ids().to_vec(),
                    self.progress.collected_item_ids().to_vec(),
                )?;
                effects.events.push(GameEvent::CheckpointReached);
                effects.progress_changed = true;
            }

            let gate_open = self
                .room
                .all_pressure_plates_held(room_tick.position, self.echo_position);
            if gate_open && !self.room.gate_open {
                effects.events.push(GameEvent::GateOpened);
            }
            self.room.gate_open = gate_open;
            if !self.pending_transition
                && gate_open
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
                    self.progress = progress_with_items(
                        next_scene.id(),
                        None,
                        completed.into_iter().collect(),
                        self.progress.collected_item_ids().to_vec(),
                    )?;
                    self.transitions.request(next_scene.id().to_owned(), tick)?;
                    self.pending_transition = true;
                    effects.progress_changed = true;
                } else if !self.completed {
                    self.progress = progress_with_items(
                        self.room.scene.id(),
                        self.progress.checkpoint_entity_id().map(str::to_owned),
                        completed.into_iter().collect(),
                        self.progress.collected_item_ids().to_vec(),
                    )?;
                    self.completed = true;
                    effects.events.push(GameEvent::Completed);
                    effects.progress_changed = true;
                }
            }
        }
        if !room_tick.hazard_contact {
            if self.input_history.len() == ECHO_HISTORY_TICKS {
                self.input_history.pop_front();
            }
            self.input_history.push_back(room_tick.position);
        }
        self.animation.advance(tick)?;
        Ok(effects)
    }
}

fn overlaps(a: [i64; 2], a_half: [i64; 2], b: [i64; 2], b_half: [i64; 2]) -> bool {
    a[0].abs_diff(b[0]) <= u64::try_from(a_half[0] + b_half[0]).unwrap_or(u64::MAX)
        && a[1].abs_diff(b[1]) <= u64::try_from(a_half[1] + b_half[1]).unwrap_or(u64::MAX)
}

fn progress_with_items(
    scene_id: impl Into<String>,
    checkpoint_id: Option<String>,
    completed_scene_ids: Vec<String>,
    collected_item_ids: Vec<String>,
) -> Result<GameProgress, SaveError> {
    GameProgress::new(scene_id, checkpoint_id, completed_scene_ids)?
        .with_collected_item_ids(collected_item_ids)
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
    chime_sound: AudioClip,
    audio_note: String,
    debug_telemetry: bool,
    started: bool,
    window: Option<WindowHandle>,
}

impl GameHost {
    fn new(
        project_root: &std::path::Path,
        recover_requested: bool,
    ) -> Result<Self, Box<dyn Error>> {
        let content = load_content(project_root)?;
        let (store, progress, save_warning) = open_progress_store(&content, recover_requested)?;
        let game = Game::new(&content, progress)?;
        let bindings = content.input.clone();
        let mapper = InputMapper::new(bindings.clone());
        let is_echo_game = content.scenes.iter().any(|(_, scene)| {
            scene
                .entities()
                .iter()
                .any(|entity| has_tag(entity, "plate"))
        });
        let mut audio = AudioService::open();
        let audio_note = match audio.status() {
            AudioStatus::Active if is_echo_game => {
                "Audio: available (moth flight and bells)".to_owned()
            }
            AudioStatus::Active => "Audio: available (jump and music)".to_owned(),
            AudioStatus::Unavailable { reason } => {
                format!("Audio: silent fallback ({})", trim_text(&reason, 48))
            }
        };
        let jump_sound = AudioClip::parse(&make_jump_wav())?;
        let chime_sound = AudioClip::parse(&make_chime_wav())?;
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
            chime_sound,
            audio_note,
            debug_telemetry: std::env::var_os("HYCEL_EDITOR_DEBUG_STREAM").is_some(),
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
                    renderer.load_texture(solid_image([53, 55, 98, 255]))?,
                );
                self.textures.insert(
                    "hazard".to_owned(),
                    renderer.load_texture(solid_image([235, 103, 154, 255]))?,
                );
                self.textures.insert(
                    "checkpoint".to_owned(),
                    renderer.load_texture(solid_image([237, 174, 73, 255]))?,
                );
                self.textures.insert(
                    "exit".to_owned(),
                    renderer.load_texture(solid_image([255, 216, 137, 255]))?,
                );
                self.textures.insert(
                    "collectible".to_owned(),
                    renderer.load_texture(solid_image([76, 201, 240, 255]))?,
                );
                self.textures.insert(
                    "gust".to_owned(),
                    renderer.load_texture(solid_image([119, 191, 244, 160]))?,
                );
                self.textures.insert(
                    "plate".to_owned(),
                    renderer.load_texture(solid_image([156, 129, 232, 255]))?,
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
            let echo_pressed = events.iter().any(|event| {
                event.action_id() == ACTION_ECHO && event.kind() == ButtonActionEventKind::Pressed
            });
            if !self.started {
                if jump_pressed || restart_pressed || echo_pressed {
                    self.started = true;
                    println!("The Bellglass night begins.");
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
                echo_pressed,
            )?;
            self.next_tick = self
                .next_tick
                .checked_add(1)
                .ok_or("simulation tick exhausted")?;
            self.report_tick_effects(&effects);
            if self.debug_telemetry && tick.is_multiple_of(6) {
                self.emit_debug_snapshot(
                    tick,
                    frame.axis(ACTION_MOVE_X),
                    jump_pressed,
                    echo_pressed,
                    restart_pressed,
                );
            }
        }
        for error in self.audio.poll_errors() {
            eprintln!("Non-fatal audio diagnostic: {error}");
        }
        Ok(())
    }

    fn report_tick_effects(&mut self, effects: &TickEffects) {
        if effects.progress_changed {
            self.persist_progress();
        }
        let ordinary_effect = effects.has(GameEvent::Jumped)
            || effects.has(GameEvent::CheckpointReached)
            || effects.has(GameEvent::EchoLaunched)
            || effects.has(GameEvent::GateOpened)
            || effects.has(GameEvent::Completed);
        if ordinary_effect
            && self.audio.play_effect(&self.jump_sound) == PlaybackOutcome::SkippedError
        {
            eprintln!("Gameplay sound could not be decoded or played");
        }
        if effects.has(GameEvent::CollectibleCollected)
            && self.audio.play_effect(&self.chime_sound) == PlaybackOutcome::SkippedError
        {
            eprintln!("Bell chime could not be decoded or played");
        }
        if effects.has(GameEvent::CheckpointReached) {
            println!(
                "Checkpoint reached: {}",
                self.game.room.checkpoint.entity_id
            );
        }
        if effects.has(GameEvent::CollectibleCollected) {
            if self.game.room.pressure_plates.is_empty() {
                println!("Collected a shard");
            } else {
                println!("Collected a bell chime");
            }
        }
        if effects.has(GameEvent::EchoLaunched) {
            println!("Echo Flight launched (up to 120 ticks)");
        }
        if effects.has(GameEvent::GateOpened) {
            println!("Echo plate held; bellglass gate open");
        }
        if effects.has(GameEvent::Died) {
            println!("Hazard hit; respawned at the active checkpoint");
        }
        if effects.has(GameEvent::SceneChanged) {
            println!("Now entering {}", self.game.room.scene.name());
        }
        if effects.has(GameEvent::Completed) {
            println!("{} is complete!", self.content.manifest.project_name());
        }
    }

    fn emit_debug_snapshot(
        &self,
        tick: u64,
        horizontal: i16,
        jump_pressed: bool,
        echo_pressed: bool,
        restart_pressed: bool,
    ) {
        let Ok(snapshot) = runtime_debug_snapshot(
            &self.game,
            tick,
            horizontal,
            jump_pressed,
            echo_pressed,
            restart_pressed,
        ) else {
            return;
        };
        let stdout = std::io::stdout();
        let mut stdout = stdout.lock();
        let _ = writeln!(stdout, "{snapshot}");
        let _ = stdout.flush();
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

    #[allow(clippy::too_many_lines)] // One deterministic scene presentation builds the bounded sprite/text lists.
    fn render_frame(&mut self) -> Result<(), Box<dyn Error>> {
        let Some(renderer) = &mut self.renderer else {
            return Ok(());
        };
        let scene = &self.game.room.scene;
        let position = self.game.room.player_position()?;
        let center = visual_pair(position)?;
        let mut sprites = night_sky_sprites(center[0], 0.38);
        for entity in scene.entities() {
            let (tag, layer) = if has_tag(entity, "platform") {
                ("platform", 0)
            } else if has_tag(entity, "hazard") {
                ("hazard", 1)
            } else if has_tag(entity, "checkpoint") {
                ("checkpoint", 2)
            } else if has_tag(entity, "gust") {
                ("gust", 0)
            } else if has_tag(entity, "plate") {
                ("plate", 2)
            } else if has_tag(entity, "exit") {
                ("exit", 3)
            } else if has_tag(entity, "collectible") && !self.game.room.collectible_collected {
                ("collectible", 3)
            } else {
                continue;
            };
            let center_milli = if has_tag(entity, "gust") {
                self.game.room.gust.map_or_else(
                    || entity.transform().translation_milli(),
                    |gust| gust.position_at(self.game.room.physics_tick),
                )
            } else {
                entity.transform().translation_milli()
            };
            let center = visual_pair(center_milli)?;
            let half = visual_pair(entity.transform().scale_milli())?;
            let texture = *self.textures.get(tag).ok_or("sample texture missing")?;
            let mut sprite = Sprite::new(texture, center, [half[0] * 2.0, half[1] * 2.0]);
            sprite.layer = layer;
            if has_tag(entity, "exit") && !self.game.room.gate_open {
                sprite.tint = [0.48, 0.34, 0.58, 1.0];
            }
            sprites.push(sprite);
        }
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

        if let Some(echo_position) = self.game.echo_position {
            let echo_center = visual_pair(echo_position)?;
            sprites.push(Sprite {
                layer: 5,
                tint: [0.42, 0.92, 1.0, 0.62],
                ..Sprite::new(
                    TextureId::WHITE,
                    echo_center,
                    [half[0] * 2.0, half[1] * 2.0],
                )
            });
        }
        let camera = Camera2D {
            center: [center[0], 0.38],
            zoom: 2.5,
        };
        let mut lines = controls_hint(scene.name(), !self.game.room.pressure_plates.is_empty());
        lines.push(self.audio_note.clone());
        if self.game.room.gust.is_some() {
            lines.push(gust_hint().to_owned());
        }
        if !self.game.room.pressure_plates.is_empty() {
            let gate_status = if self.game.room.gate_open {
                "Bellglass gate: OPEN"
            } else {
                "Bellglass gate: SEALED - courier and echo must hold every violet plate"
            };
            lines.push(gate_status.to_owned());
            lines.push(format!(
                "Bell chimes: {}/3   Echo remaining: {} ticks",
                self.game.progress.collected_item_ids().len(),
                self.game.echo.as_ref().map_or(0, |echo| {
                    echo.positions.len().saturating_sub(echo.next_position)
                })
            ));
        }
        if !self.started {
            let objective = if self.game.room.pressure_plates.is_empty() {
                "Collect the shards, avoid hazards, and reach the exit."
            } else {
                "Record a flight, hold each violet plate with the echo, then cross."
            };
            lines.push(objective.to_owned());
            lines.push("Press Space to start.".to_owned());
        } else if self.game.completed {
            lines.push("The journey is complete! Press R to play again.".to_owned());
        }
        if let Some(warning) = &self.save_warning {
            lines.push(format!("Progress warning: {}", trim_text(warning, 120)));
        }
        let texts = lines
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let index = u16::try_from(index).unwrap_or(u16::MAX);
                let mut debug_text = DebugText::new([14, 22 + u32::from(index) * 22], text.clone());
                debug_text.scale = 2;
                debug_text
            })
            .collect::<Vec<_>>();
        match renderer.render_scene(camera, &sprites, &texts)? {
            FrameOutcome::Presented | FrameOutcome::Skipped | FrameOutcome::Reconfigured => {}
        }
        Ok(())
    }
}

fn night_sky_sprites(camera_x: f32, camera_y: f32) -> Vec<Sprite> {
    const SKYLINE: [(f32, f32, f32, f32); 7] = [
        (-0.58, 0.49, 0.14, 0.20),
        (-0.40, 0.53, 0.19, 0.13),
        (-0.18, 0.47, 0.16, 0.24),
        (0.02, 0.52, 0.19, 0.15),
        (0.24, 0.48, 0.15, 0.22),
        (0.43, 0.54, 0.17, 0.12),
        (0.61, 0.50, 0.14, 0.19),
    ];
    const STARS: [(f32, f32, f32); 12] = [
        (-0.56, -0.28, 0.010),
        (-0.43, -0.16, 0.007),
        (-0.31, -0.32, 0.008),
        (-0.17, -0.22, 0.011),
        (-0.04, -0.34, 0.007),
        (0.08, -0.19, 0.009),
        (0.19, -0.30, 0.007),
        (0.31, -0.18, 0.010),
        (0.42, -0.33, 0.008),
        (0.54, -0.23, 0.007),
        (-0.52, -0.06, 0.006),
        (0.25, -0.06, 0.006),
    ];
    let mut sprites = Vec::with_capacity(SKYLINE.len() + STARS.len() + 8);
    for (index, (offset_x, y, width, height)) in SKYLINE.iter().copied().enumerate() {
        let mut building = Sprite::new(
            TextureId::WHITE,
            [camera_x * 0.14 + offset_x, y],
            [width, height],
        );
        building.layer = -8;
        building.order = i32::try_from(index).unwrap_or(i32::MAX);
        building.tint = [0.10, 0.13, 0.24, 1.0];
        sprites.push(building);
    }
    for (index, (offset_x, offset_y, size)) in STARS.iter().copied().enumerate() {
        let mut star = Sprite::new(
            TextureId::WHITE,
            [camera_x * 0.35 + offset_x, camera_y + offset_y],
            [size, size],
        );
        star.layer = -7;
        star.order = i32::try_from(index).unwrap_or(i32::MAX);
        star.tint = if index % 3 == 0 {
            [0.95, 0.74, 0.52, 1.0]
        } else {
            [0.55, 0.77, 0.95, 1.0]
        };
        sprites.push(star);
    }
    for (index, (x, y)) in [
        (-0.56, 0.49),
        (-0.39, 0.52),
        (-0.17, 0.46),
        (0.04, 0.51),
        (0.24, 0.47),
        (0.42, 0.53),
        (0.62, 0.50),
        (-0.12, 0.53),
    ]
    .into_iter()
    .enumerate()
    {
        let mut window = Sprite::new(TextureId::WHITE, [camera_x * 0.14 + x, y], [0.012, 0.022]);
        window.layer = -6;
        window.order = i32::try_from(index).unwrap_or(i32::MAX);
        window.tint = if index % 2 == 0 {
            [0.89, 0.60, 0.31, 1.0]
        } else {
            [0.38, 0.65, 0.77, 1.0]
        };
        sprites.push(window);
    }
    sprites
}

fn gust_hint() -> &'static str {
    "Clockwork Gust sweeps side-to-side across the raised platform."
}

fn runtime_debug_snapshot(
    game: &Game,
    tick: u64,
    horizontal: i16,
    jump_pressed: bool,
    echo_pressed: bool,
    restart_pressed: bool,
) -> Result<serde_json::Value, Box<dyn Error>> {
    let body = game.room.physics.body_state(game.room.player)?;
    let echo_remaining_ticks = game.echo.as_ref().map_or(0, |echo| {
        echo.positions.len().saturating_sub(echo.next_position)
    });
    Ok(serde_json::json!({
        "kind": "hycel_runtime_debug",
        "schema_version": 1,
        "tick": tick,
        "scene_tick": game.room.physics_tick,
        "scene_id": game.room.scene.id(),
        "scene_name": game.room.scene.name(),
        "player_position_milli": [body.position.x.milli_units(), body.position.y.milli_units()],
        "player_velocity_milli": [body.velocity.x.milli_units(), body.velocity.y.milli_units()],
        "grounded": game.room.grounded,
        "move_axis": horizontal,
        "jump_pressed": jump_pressed,
        "echo_pressed": echo_pressed,
        "restart_pressed": restart_pressed,
        "echo_remaining_ticks": echo_remaining_ticks,
        "gate_open": game.room.gate_open,
        "collected_chime_count": game.progress.collected_item_ids().len(),
        "completed": game.completed,
    }))
}

fn controls_hint(scene_name: &str, has_echo_puzzle: bool) -> Vec<String> {
    let mut lines = vec![format!("{scene_name} | A/D or arrows: move | Space: jump")];
    if has_echo_puzzle {
        lines.push("E: echo last 120 ticks | R: checkpoint".to_owned());
    } else {
        lines.push("R: checkpoint".to_owned());
    }
    lines
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
        format!("{prefix}...")
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

fn make_chime_wav() -> Vec<u8> {
    let sample_count = WAVE_SAMPLE_RATE / 4;
    let data_bytes = sample_count * 2;
    let mut wav = Vec::with_capacity(44 + usize::try_from(data_bytes).unwrap_or(0));
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
        let envelope = i32::try_from((sample_count - sample) * 12_000 / sample_count).unwrap_or(0);
        let phase = (sample * 880 / WAVE_SAMPLE_RATE) % 2;
        let amplitude = if phase == 0 { envelope } else { -envelope };
        wav.extend_from_slice(&i16::try_from(amplitude).unwrap_or_default().to_le_bytes());
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScenarioOutcome {
    pub name: String,
    pub passed: bool,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayOutcome {
    pub frame_count: usize,
    pub scene_id: String,
    pub player_position_milli: [i64; 2],
    pub completed: bool,
    pub state_hash: String,
}

#[derive(Clone)]
#[allow(clippy::struct_excessive_bools)] // Independent canonical gameplay flags are serialized explicitly.
struct ReplayState {
    scene_id: String,
    progress_scene_id: String,
    player_position_milli: [i64; 2],
    player_velocity_milli: [i64; 2],
    physics_tick: u64,
    grounded: bool,
    pending_transition: bool,
    collectible_collected: bool,
    checkpoint_id: String,
    completed_scene_ids: Vec<String>,
    collected_item_ids: Vec<String>,
    input_history: Vec<[i64; 2]>,
    echo_positions: Vec<[i64; 2]>,
    echo_next_position: usize,
    echo_position: Option<[i64; 2]>,
    gate_open: bool,
    completed: bool,
}

impl CanonicalState for ReplayState {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.write_str(&self.scene_id);
        writer.write_str(&self.progress_scene_id);
        writer.write_i64(self.player_position_milli[0]);
        writer.write_i64(self.player_position_milli[1]);
        writer.write_i64(self.player_velocity_milli[0]);
        writer.write_i64(self.player_velocity_milli[1]);
        writer.write_u64(self.physics_tick);
        writer.write_bool(self.grounded);
        writer.write_bool(self.pending_transition);
        writer.write_bool(self.collectible_collected);
        writer.write_str(&self.checkpoint_id);
        writer.write_sequence_len(self.completed_scene_ids.len());
        for scene_id in &self.completed_scene_ids {
            writer.write_str(scene_id);
        }
        writer.write_sequence_len(self.collected_item_ids.len());
        for item_id in &self.collected_item_ids {
            writer.write_str(item_id);
        }
        writer.write_sequence_len(self.input_history.len());
        for position in &self.input_history {
            writer.write_i64(position[0]);
            writer.write_i64(position[1]);
        }
        writer.write_sequence_len(self.echo_positions.len());
        for position in &self.echo_positions {
            writer.write_i64(position[0]);
            writer.write_i64(position[1]);
        }
        writer.write_u64(u64::try_from(self.echo_next_position).unwrap_or(u64::MAX));
        writer.write_bool(self.echo_position.is_some());
        if let Some(position) = self.echo_position {
            writer.write_i64(position[0]);
            writer.write_i64(position[1]);
        }
        writer.write_bool(self.gate_open);
        writer.write_bool(self.completed);
    }
}

const SCENARIO_NAMES: [&str; 6] = [
    "authored-content",
    "fixed-tick-gameplay",
    "echo-flight",
    "hazard-respawn",
    "platform-support",
    "audio-decode",
];

/// Runs the fixed, bounded headless scenario suite for one validated game project.
///
/// # Errors
///
/// Returns an error when project files cannot be loaded or a requested scenario
/// name is unknown. Individual scenario assertion failures are returned as outcomes.
pub fn run_scenarios(
    project_root: &std::path::Path,
    selected: Option<&str>,
) -> Result<Vec<ScenarioOutcome>, Box<dyn Error>> {
    let content = load_content(project_root)?;
    let has_echo_plate = content.scenes.iter().any(|(_, scene)| {
        scene
            .entities()
            .iter()
            .any(|entity| has_tag(entity, "plate"))
    });
    let names = SCENARIO_NAMES
        .iter()
        .copied()
        .filter(|name| *name != "echo-flight" || has_echo_plate)
        .collect::<Vec<_>>();
    if let Some(selected) = selected {
        if !names.contains(&selected) {
            return Err(format!(
                "unknown scenario {selected:?}; available scenarios: {}",
                names.join(", ")
            )
            .into());
        }
    }
    let names = selected.map_or_else(|| names.clone(), |name| vec![name]);
    Ok(names
        .iter()
        .map(|name| {
            let result = run_scenario(name, &content);
            ScenarioOutcome {
                name: (*name).to_owned(),
                passed: result.is_ok(),
                message: match result {
                    Ok(()) => "scenario assertions passed".to_owned(),
                    Err(error) => error,
                },
            }
        })
        .collect())
}

/// Plays a bounded tick-indexed replay through the compiled-in reference game.
///
/// # Errors
///
/// Returns an error for invalid/incompatible replay metadata, invalid project
/// data, unavailable target support, or a failed simulation tick.
pub fn replay_project(
    project_root: &std::path::Path,
    replay_bytes: &[u8],
) -> Result<ReplayOutcome, Box<dyn Error>> {
    let replay = Replay::from_json(replay_bytes)?;
    let header = replay.header();
    let current_target = current_target_triple().ok_or("replay is not available on this target")?;
    if header.engine_version() != env!("CARGO_PKG_VERSION") {
        return Err(format!(
            "replay engine version {} does not match this runtime {}",
            header.engine_version(),
            env!("CARGO_PKG_VERSION")
        )
        .into());
    }
    if header.target_triple() != current_target {
        return Err(format!(
            "replay target {} does not match this runtime {current_target}",
            header.target_triple()
        )
        .into());
    }
    if header.ticks_per_second() != 60 || header.seed() != 0 || header.stream() != 0 {
        return Err("reference game replay requires 60 Hz and seed/stream 0/0".into());
    }
    let content = load_content(project_root)?;
    let progress = initial_progress(&content)?;
    let mut game = Game::new(&content, progress)?;
    let mut previous_jump = false;
    let mut previous_restart = false;
    let mut previous_echo = false;
    for frame in replay.frames() {
        let jump = frame.button(ACTION_JUMP);
        let restart = frame.button(ACTION_RESTART);
        let echo = frame.button(ACTION_ECHO);
        let effects = game.tick(
            frame.tick(),
            frame.axis(ACTION_MOVE_X),
            jump && !previous_jump,
            restart && !previous_restart,
            echo && !previous_echo,
        )?;
        let _ = effects;
        previous_jump = jump;
        previous_restart = restart;
        previous_echo = echo;
    }
    let body = game.room.physics.body_state(game.room.player)?;
    let player_position_milli = [body.position.x.milli_units(), body.position.y.milli_units()];
    let state = ReplayState {
        scene_id: game.room.scene.id().to_owned(),
        progress_scene_id: game.progress.current_scene_id().to_owned(),
        player_position_milli,
        player_velocity_milli: [body.velocity.x.milli_units(), body.velocity.y.milli_units()],
        physics_tick: game.room.physics_tick,
        grounded: game.room.grounded,
        pending_transition: game.pending_transition,
        collectible_collected: game.room.collectible_collected,
        checkpoint_id: game
            .progress
            .checkpoint_entity_id()
            .unwrap_or_default()
            .to_owned(),
        completed_scene_ids: game.progress.completed_scene_ids().to_vec(),
        collected_item_ids: game.progress.collected_item_ids().to_vec(),
        input_history: game.input_history.iter().copied().collect(),
        echo_positions: game
            .echo
            .as_ref()
            .map_or_else(Vec::new, |echo| echo.positions.clone()),
        echo_next_position: game.echo.as_ref().map_or(0, |echo| echo.next_position),
        echo_position: game.echo_position,
        gate_open: game.room.gate_open,
        completed: game.completed,
    };
    Ok(ReplayOutcome {
        frame_count: replay.frames().len(),
        scene_id: state.scene_id.clone(),
        player_position_milli,
        completed: state.completed,
        state_hash: state.state_hash().to_hex(),
    })
}

fn current_target_triple() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        ("windows", "aarch64") => Some("aarch64-pc-windows-msvc"),
        _ => None,
    }
}

fn scenario_input(content: &Content, tick: u64) -> (i16, bool, bool) {
    let has_echo_plate = content.scenes.iter().any(|(_, scene)| {
        scene
            .entities()
            .iter()
            .any(|entity| has_tag(entity, "plate"))
    });
    if !has_echo_plate {
        return (i16::MAX, matches!(tick, 10 | 55 | 95 | 140), false);
    }
    let phase = tick % 120;
    let horizontal = if phase < 28 || (55..63).contains(&phase) {
        i16::MAX
    } else {
        0
    };
    (horizontal, phase == 10, phase == 55)
}

#[allow(clippy::too_many_lines)] // Keeps the small, named headless scenario assertions together.
fn run_scenario(name: &str, content: &Content) -> Result<(), String> {
    match name {
        "authored-content" => {
            let courier_layout_valid = content.scenes.len() != 3
                || (content.scenes[0]
                    .1
                    .entities()
                    .iter()
                    .filter(|entity| has_tag(entity, "plate"))
                    .count()
                    == 1
                    && content.scenes[1]
                        .1
                        .entities()
                        .iter()
                        .any(|entity| has_tag(entity, "gust"))
                    && content.scenes[2]
                        .1
                        .entities()
                        .iter()
                        .filter(|entity| has_tag(entity, "plate"))
                        .count()
                        == 2);
            if !(2..=3).contains(&content.scenes.len())
                || !courier_layout_valid
                || content.animation.frames().len() != 2
                || content.input.axes().len() != 1
                || content.texture_bytes.len() != 2
            {
                return Err(
                    "authored scenes, animation, input, and textures are not connected as expected"
                        .to_owned(),
                );
            }
            Ok(())
        }
        "fixed-tick-gameplay" => {
            let mut game = Game::new(
                content,
                initial_progress(content).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            let mut observed = Vec::new();
            let mut final_gate_opened = false;
            for tick in 0..360 {
                let (horizontal, jump, echo) = scenario_input(content, game.room.physics_tick);
                let effects = game
                    .tick(tick, horizontal, jump, false, echo)
                    .map_err(|error| error.to_string())?;
                if content.scenes.len() == 3
                    && game.room.scene.id() == content.scenes[2].1.id()
                    && effects.has(GameEvent::GateOpened)
                {
                    final_gate_opened = true;
                }
                observed.extend(effects.events);
            }
            if content.scenes.len() == 3 && !final_gate_opened {
                return Err(
                    "final two-plate gate did not open during the complete gameplay route"
                        .to_owned(),
                );
            }
            let required = [
                GameEvent::Jumped,
                GameEvent::CollectibleCollected,
                GameEvent::CheckpointReached,
                GameEvent::SceneChanged,
                GameEvent::Completed,
            ];
            if required.iter().any(|event| !observed.contains(event)) || !game.completed {
                return Err("expected movement, jump, shard, checkpoint, room transition, and completion outcomes".to_owned());
            }
            let restored =
                Game::new(content, game.progress.clone()).map_err(|error| error.to_string())?;
            if !restored.completed {
                return Err("completed progress did not restore the completed state".to_owned());
            }
            Ok(())
        }
        "echo-flight" => {
            let mut game = Game::new(
                content,
                initial_progress(content).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            let mut launched = false;
            let mut opened_after_launch = false;
            for tick in 0..120 {
                let (horizontal, jump, echo) = scenario_input(content, game.room.physics_tick);
                let effects = game
                    .tick(tick, horizontal, jump, false, echo)
                    .map_err(|error| error.to_string())?;
                launched |= effects.has(GameEvent::EchoLaunched);
                opened_after_launch |= launched && effects.has(GameEvent::GateOpened);
                if effects.has(GameEvent::SceneChanged) {
                    break;
                }
            }
            if !launched || !opened_after_launch || game.room.scene.id() == content.scenes[0].1.id()
            {
                return Err(
                    "a recorded echo must hold the authored plate and open the first gate"
                        .to_owned(),
                );
            }
            Ok(())
        }
        "hazard-respawn" => {
            let mut game = Game::new(
                content,
                initial_progress(content).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            let mut died = false;
            for tick in 0..40 {
                died |= game
                    .tick(tick, i16::MAX, false, false, false)
                    .map_err(|error| error.to_string())?
                    .has(GameEvent::Died);
            }
            if !died {
                return Err("walking into the authored hazard did not trigger respawn".to_owned());
            }
            Ok(())
        }
        "platform-support" => {
            let room = Room::new(content.scenes[1].1.clone(), None, &[])
                .map_err(|error| error.to_string())?;
            if !room.has_ground_support([40, 400]) || room.has_ground_support([40, 600]) {
                return Err(
                    "platform support must include its top side and exclude its underside"
                        .to_owned(),
                );
            }
            Ok(())
        }
        "audio-decode" => {
            let effect = AudioClip::parse(&make_jump_wav()).map_err(|error| error.to_string())?;
            let chime = AudioClip::parse(&make_chime_wav()).map_err(|error| error.to_string())?;
            let music = AudioClip::parse(&make_music_wav()).map_err(|error| error.to_string())?;
            if effect.encoded_len() == 0
                || chime.encoded_len() <= effect.encoded_len()
                || music.encoded_len() <= chime.encoded_len()
            {
                return Err(
                    "generated effect and music clips failed the bounded WAV check".to_owned(),
                );
            }
            Ok(())
        }
        _ => Err(format!("unknown scenario {name:?}")),
    }
}

/// Launches the windowed compiled-in reference game using an explicit project root.
///
/// # Errors
///
/// Returns an error for incompatible project/save data or native runtime startup failure.
pub fn run_project(
    project_root: &std::path::Path,
    recover_requested: bool,
) -> Result<(), Box<dyn Error>> {
    let mut host = GameHost::new(project_root, recover_requested)?;
    if let Some(warning) = &host.save_warning {
        eprintln!("{warning}");
    }
    let failure = Rc::new(RefCell::new(None::<String>));
    let callback_failure = Rc::clone(&failure);
    let config = WindowConfig::new(host.content.manifest.project_name(), 960, 540)?;
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

fn main() -> Result<(), Box<dyn Error>> {
    let recover_requested = std::env::args().any(|argument| argument == "--recover-save");
    let project_root = find_content_root()?;
    run_project(&project_root, recover_requested)
}

#[cfg(test)]
mod tests {
    use super::{
        Game, GameProgress, ReplayState, find_content_root, initial_progress, load_content,
        make_chime_wav, make_jump_wav, make_music_wav, runtime_debug_snapshot,
    };
    use hycel_audio::AudioClip;
    use hycel_core::CanonicalState;
    use hycel_input::{InputEvent, InputMapper, KeyCode};

    fn load_sample_content() -> super::Content {
        let root = find_content_root().unwrap();
        load_content(&root).unwrap()
    }

    #[test]
    fn replay_hash_includes_future_determining_physics_state() {
        let base = ReplayState {
            scene_id: "scene-a".to_owned(),
            progress_scene_id: "scene-a".to_owned(),
            player_position_milli: [0, 0],
            player_velocity_milli: [0, 0],
            physics_tick: 12,
            grounded: true,
            pending_transition: false,
            collectible_collected: false,
            checkpoint_id: String::new(),
            completed_scene_ids: Vec::new(),
            collected_item_ids: Vec::new(),
            input_history: Vec::new(),
            echo_positions: Vec::new(),
            echo_next_position: 0,
            echo_position: None,
            gate_open: false,
            completed: false,
        };
        let expected = base.state_hash();

        let mut different_tick = base.clone();
        different_tick.physics_tick += 1;
        assert_ne!(expected, different_tick.state_hash());

        let mut different_velocity = base.clone();
        different_velocity.player_velocity_milli[0] = 1;
        assert_ne!(expected, different_velocity.state_hash());

        let mut different_grounding = base.clone();
        different_grounding.grounded = false;
        assert_ne!(expected, different_grounding.state_hash());

        let mut different_history = base.clone();
        different_history.input_history.push([1, 2]);
        assert_ne!(expected, different_history.state_hash());

        let mut different_echo = base.clone();
        different_echo.echo_positions.push([3, 4]);
        assert_ne!(expected, different_echo.state_hash());

        let mut different_echo_cursor = different_echo.clone();
        different_echo_cursor.echo_next_position = 1;
        assert_ne!(
            different_echo.state_hash(),
            different_echo_cursor.state_hash()
        );

        let mut different_echo_position = base.clone();
        different_echo_position.echo_position = Some([3, 4]);
        assert_ne!(expected, different_echo_position.state_hash());

        let mut different_gate = base;
        different_gate.gate_open = true;
        assert_ne!(expected, different_gate.state_hash());
    }

    #[test]
    fn gust_hint_explains_the_middle_room_obstacle() {
        assert!(super::gust_hint().contains("side-to-side"));
        assert!(super::gust_hint().contains("raised platform"));
    }

    #[test]
    fn night_sky_backdrop_is_bounded_layered_and_parallax_stable() {
        let near = super::night_sky_sprites(0.0, 0.38);
        let farther = super::night_sky_sprites(10.0, 0.38);
        assert_eq!(near.len(), farther.len());
        assert!(!near.is_empty());
        assert!(near.iter().all(|sprite| sprite.layer < 0));
        assert!(near.iter().all(|sprite| {
            sprite.size[0] > 0.0
                && sprite.size[1] > 0.0
                && sprite
                    .tint
                    .iter()
                    .all(|channel| (0.0..=1.0).contains(channel))
        }));
        assert!((farther[0].center[0] - near[0].center[0] - 1.4).abs() < 0.0001);
        assert!((farther[7].center[0] - near[7].center[0] - 3.5).abs() < 0.0001);
    }

    #[test]
    fn runtime_debug_snapshot_reports_versioned_tick_input_and_game_state() {
        let content = load_sample_content();
        let game = Game::new(&content, initial_progress(&content).unwrap()).unwrap();
        let snapshot = runtime_debug_snapshot(&game, 0, 1, true, false, false).unwrap();
        assert_eq!(snapshot["kind"], "hycel_runtime_debug");
        assert_eq!(snapshot["schema_version"], 1);
        assert_eq!(snapshot["tick"], 0);
        assert_eq!(snapshot["scene_tick"], 0);
        assert_eq!(snapshot["move_axis"], 1);
        assert_eq!(snapshot["jump_pressed"], true);
        assert_eq!(snapshot["echo_remaining_ticks"], 0);
        assert_eq!(snapshot["scene_id"], game.room.scene.id());
        assert_eq!(snapshot["gate_open"], game.room.gate_open);
    }

    #[test]
    fn courier_hud_keyboard_controls_match_authored_physical_bindings() {
        let content = load_sample_content();
        let mut mapper = InputMapper::new(content.input.clone());
        for (code, expected_axis) in [
            (KeyCode::KeyA, i16::MIN),
            (KeyCode::ArrowLeft, i16::MIN),
            (KeyCode::KeyD, i16::MAX),
            (KeyCode::ArrowRight, i16::MAX),
        ] {
            mapper.handle_event(InputEvent::Key {
                code,
                pressed: true,
                synthetic: false,
            });
            assert_eq!(mapper.frame(0).axis(super::ACTION_MOVE_X), expected_axis);
            mapper.handle_event(InputEvent::Key {
                code,
                pressed: false,
                synthetic: false,
            });
        }
        for (code, action_id) in [
            (KeyCode::Space, super::ACTION_JUMP),
            (KeyCode::KeyR, super::ACTION_RESTART),
            (KeyCode::KeyE, super::ACTION_ECHO),
        ] {
            mapper.handle_event(InputEvent::Key {
                code,
                pressed: true,
                synthetic: false,
            });
            assert!(mapper.frame(1).button(action_id));
            mapper.handle_event(InputEvent::Key {
                code,
                pressed: false,
                synthetic: false,
            });
        }
    }

    #[test]
    fn gameplay_hud_names_the_supported_keyboard_controls_in_both_room_types() {
        let introductory = super::controls_hint("Mosslight Roofs", false);
        assert!(introductory[0].contains("A/D or arrows: move"));
        assert!(introductory[0].contains("Space: jump"));
        assert_eq!(introductory.len(), 2);
        assert!(!introductory.iter().any(|line| line.contains("E: echo")));

        let puzzle = super::controls_hint("Moonworks", true);
        assert!(puzzle[0].contains("A/D or arrows: move"));
        assert!(puzzle[0].contains("Space: jump"));
        assert!(puzzle[1].contains("E: echo last 120 ticks"));
        assert!(puzzle[1].contains("R: checkpoint"));
    }

    #[test]
    fn authored_bellglass_courier_content_is_strict_and_connected() {
        let content = load_sample_content();
        assert_eq!(content.scenes.len(), 3);
        assert_eq!(content.animation.frames().len(), 2);
        assert_eq!(content.input.axes().len(), 1);
        assert_eq!(content.texture_bytes.len(), 2);
    }

    #[test]
    fn fixed_tick_controls_move_and_jump_without_a_window() {
        let content = load_sample_content();
        let progress = initial_progress(&content).unwrap();
        let mut game = Game::new(&content, progress).unwrap();
        let mut saw_jump = false;
        let mut saw_collectible = false;
        let mut saw_checkpoint = false;
        let mut saw_transition = false;
        let mut saw_completion = false;
        for tick in 0..360 {
            let (horizontal, jump, echo) = super::scenario_input(&content, game.room.physics_tick);
            let effects = game.tick(tick, horizontal, jump, false, echo).unwrap();
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
            "completion not reached: scene={} position={:?} gate={} completed={:?}",
            game.room.scene.name(),
            game.room.player_position().unwrap(),
            game.room.gate_open,
            game.progress.completed_scene_ids()
        );
        assert_eq!(game.room.scene.id(), content.scenes[2].1.id());
        assert!(
            game.progress
                .completed_scene_ids()
                .contains(&content.scenes[2].1.id().to_owned())
        );
        assert!(game.completed);
        assert_eq!(game.progress.collected_item_ids().len(), 3);
        let restored = Game::new(&content, game.progress.clone()).unwrap();
        assert!(
            restored.completed,
            "completion should survive process restarts"
        );
        assert!(
            restored.room.collectible_collected,
            "the current room's bell chime should remain collected after loading"
        );
        assert!(!game.animation.texture_id().is_empty());
    }

    #[test]
    fn echo_history_is_bounded_and_playback_expires_on_its_last_recorded_tick() {
        let content = load_sample_content();
        let progress = initial_progress(&content).unwrap();
        let mut game = Game::new(&content, progress).unwrap();
        for tick in 0..130 {
            game.tick(tick, 0, false, false, false).unwrap();
        }
        assert_eq!(game.input_history.len(), super::ECHO_HISTORY_TICKS);

        let launched = game.tick(130, 0, false, false, true).unwrap();
        assert!(launched.has(super::GameEvent::EchoLaunched));
        let echo = game.echo.as_ref().unwrap();
        assert_eq!(echo.positions.len(), super::ECHO_HISTORY_TICKS);
        assert_eq!(echo.next_position, 1);
        assert!(game.echo_position.is_some());

        for tick in 131..249 {
            game.tick(tick, 0, false, false, false).unwrap();
        }
        assert!(game.echo.is_some());
        let final_playback_tick = game.tick(249, 0, false, false, false).unwrap();
        assert!(game.echo.is_none());
        assert!(game.echo_position.is_some());
        assert!(!final_playback_tick.has(super::GameEvent::EchoLaunched));
        game.tick(250, 0, false, false, false).unwrap();
        assert!(game.echo_position.is_none());
        game.tick(251, 0, false, false, false).unwrap();
        game.tick(252, 0, false, false, false).unwrap();
        game.tick(253, 0, false, false, true).unwrap();
        assert!(game.echo.is_some());
        game.tick(254, 0, false, true, false).unwrap();
        assert!(game.echo.is_none());
        assert!(game.echo_position.is_none());
        assert_eq!(game.input_history.len(), 1);
    }

    #[test]
    fn only_top_side_platform_contacts_enable_jumping() {
        let content = load_sample_content();
        let room = super::Room::new(content.scenes[1].1.clone(), None, &[]).unwrap();
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
    fn saved_item_references_are_resolved_against_authored_scenes() {
        let content = load_sample_content();
        let progress = GameProgress::new(content.scenes[0].1.id(), None, Vec::new())
            .unwrap()
            .with_collected_item_ids(vec!["00000000-0000-4000-8000-000000000099".to_owned()])
            .unwrap();
        assert!(Game::new(&content, progress).is_err());
    }

    #[test]
    fn final_room_requires_player_and_echo_to_hold_both_separated_plates() {
        let content = load_sample_content();
        let final_scene = &content.scenes[2].1;
        let room = super::Room::new(final_scene.clone(), None, &[]).unwrap();
        assert_eq!(room.pressure_plates.len(), 2);
        let first = room.pressure_plates[0].position;
        let second = room.pressure_plates[1].position;
        assert!(!room.all_pressure_plates_held(second, None));
        assert!(!room.all_pressure_plates_held(first, None));
        assert!(room.all_pressure_plates_held(second, Some(first)));
    }

    #[test]
    fn recorded_echo_opens_the_authored_puzzle_gate_headlessly() {
        let content = load_sample_content();
        assert!(super::run_scenario("echo-flight", &content).is_ok());
    }

    #[test]
    fn hazard_contact_respawns_at_the_active_checkpoint() {
        let content = load_sample_content();
        let progress = initial_progress(&content).unwrap();
        let mut game = Game::new(&content, progress).unwrap();
        let mut died = false;
        for tick in 0..40 {
            let effects = game.tick(tick, i16::MAX, false, false, false).unwrap();
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
        let chime = AudioClip::parse(&make_chime_wav()).unwrap();
        let music = AudioClip::parse(&make_music_wav()).unwrap();
        assert!(chime.encoded_len() > clip.encoded_len());
        assert!(music.encoded_len() > chime.encoded_len());
    }
}
