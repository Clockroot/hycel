//! Backend-independent, tick-stepped 2D box physics for small platform games.
//!
//! The public API uses [`hycel_core`] fixed-point types and stable Hycel body IDs;
//! Rapier types remain private to this crate. The backend uses Rapier's
//! `enhanced-determinism` feature and is stepped serially at a configured fixed
//! tick rate. Determinism guarantees remain scoped to a fixed Rapier/compiler/
//! platform configuration until broader replay tests establish otherwise.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};

use hycel_core::{SimScalar, Vec2};
use rapier2d::prelude::{
    BroadPhaseBvh, CCDSolver, ColliderBuilder, ColliderHandle, ColliderSet, ImpulseJointSet,
    IntegrationParameters, IslandManager, MultibodyJointSet, NarrowPhase, PhysicsPipeline,
    RigidBodyBuilder, RigidBodyHandle, RigidBodySet, Vector,
};

/// Maximum number of box bodies in one physics world.
pub const MAX_BODIES: usize = 1_024;
/// Maximum absolute coordinate or velocity, in milli-world-units, accepted by the adapter.
pub const MAX_ABS_MILLI_UNITS: i64 = 10_000_000;
const MAX_TICK_RATE: u32 = 1_000;

/// Stable, monotonically allocated identity for a body in one physics world.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BodyId(u64);

impl BodyId {
    /// Numeric body identity, stable for the lifetime of its physics world.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Motion type for an axis-aligned box body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyKind {
    /// Immovable body, suitable for floors and walls.
    Fixed,
    /// Solver-integrated body affected by gravity and contact forces.
    Dynamic,
    /// User-controlled body moved from its prescribed linear velocity.
    Kinematic,
}

/// Validated-at-insertion description of one box body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoxBody {
    kind: BodyKind,
    position: Vec2,
    half_extents: Vec2,
    velocity: Vec2,
    continuous_collision_detection: bool,
}

impl BoxBody {
    /// Creates an axis-aligned box with the given center and positive half-extents.
    #[must_use]
    pub const fn new(kind: BodyKind, position: Vec2, half_extents: Vec2) -> Self {
        Self {
            kind,
            position,
            half_extents,
            velocity: Vec2::ZERO,
            continuous_collision_detection: false,
        }
    }

    /// Sets initial or prescribed linear velocity in world units per second.
    #[must_use]
    pub const fn with_velocity(mut self, velocity: Vec2) -> Self {
        self.velocity = velocity;
        self
    }

    /// Enables continuous collision detection for a dynamic body.
    #[must_use]
    pub const fn with_continuous_collision_detection(mut self) -> Self {
        self.continuous_collision_detection = true;
        self
    }
}

/// Fixed tick and gravity configuration for a physics world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicsConfig {
    tick_rate_hz: u32,
    gravity: Vec2,
}

impl PhysicsConfig {
    /// Creates a configuration. Tick rates must be in `1..=1000`.
    #[must_use]
    pub const fn new(tick_rate_hz: u32, gravity: Vec2) -> Self {
        Self {
            tick_rate_hz,
            gravity,
        }
    }

    /// Fixed simulation ticks per second.
    #[must_use]
    pub const fn tick_rate_hz(self) -> u32 {
        self.tick_rate_hz
    }

    /// Acceleration applied to dynamic bodies in world units per second squared.
    #[must_use]
    pub const fn gravity(self) -> Vec2 {
        self.gravity
    }
}

impl Default for PhysicsConfig {
    fn default() -> Self {
        Self::new(
            60,
            Vec2::new(SimScalar::ZERO, SimScalar::from_milli_units(9_800)),
        )
    }
}

/// Read-only fixed-point position and velocity of a body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyState {
    /// Body center in world units.
    pub position: Vec2,
    /// Linear velocity in world units per second.
    pub velocity: Vec2,
}

/// Whether a body pair began or ended a contact on the latest physics tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ContactKind {
    /// The pair was not touching on the previous tick and is touching now.
    Started,
    /// The pair was touching on the previous tick and is no longer touching.
    Stopped,
}

/// A canonicalized pair of stable body IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ContactPair {
    /// Lower body ID in the pair.
    pub first: BodyId,
    /// Higher body ID in the pair.
    pub second: BodyId,
}

/// A contact transition, ordered by body pair and transition kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ContactEvent {
    /// Canonical body pair.
    pub pair: ContactPair,
    /// Contact transition.
    pub kind: ContactKind,
}

/// Physics configuration, geometry, identity, or tick error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicsError {
    /// Tick rate is zero or exceeds the documented limit.
    InvalidTickRate,
    /// A coordinate, extent, velocity, or gravity component is out of range.
    ValueOutOfRange,
    /// A box has a zero or negative half-extent.
    InvalidGeometry,
    /// Continuous collision detection was requested for a non-dynamic body.
    CcdRequiresDynamicBody,
    /// A velocity was assigned to an immovable body.
    FixedBodyVelocity,
    /// The body limit has been reached.
    BodyLimitReached,
    /// No body with the given ID exists in this world.
    UnknownBody,
    /// The requested tick is not the next contiguous tick (the first is zero).
    NonContiguousTick,
    /// The world exhausted its `u64` tick sequence.
    TickExhausted,
    /// The world exhausted its monotonically allocated body IDs.
    BodyIdsExhausted,
    /// A backend value became non-finite or left the adapter's fixed-point range.
    InvalidBackendState,
}

impl fmt::Display for PhysicsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidTickRate => "physics tick rate must be in 1..=1000 Hz",
            Self::ValueOutOfRange => "physics value exceeds the adapter's fixed-point range",
            Self::InvalidGeometry => "box half-extents must be positive",
            Self::CcdRequiresDynamicBody => {
                "continuous collision detection requires a dynamic body"
            }
            Self::FixedBodyVelocity => "fixed bodies cannot be assigned a velocity",
            Self::BodyLimitReached => "physics world reached its body limit",
            Self::UnknownBody => "physics body ID does not exist in this world",
            Self::NonContiguousTick => "physics ticks must start at zero and be contiguous",
            Self::TickExhausted => "physics world exhausted its tick sequence",
            Self::BodyIdsExhausted => "physics world exhausted its body ID sequence",
            Self::InvalidBackendState => {
                "physics backend produced an invalid or out-of-range state"
            }
        };
        formatter.write_str(message)
    }
}

impl Error for PhysicsError {}

/// A serially stepped Rapier world hidden behind Hycel-owned types.
///
/// The world is cloneable for simulation rollback. Clones copy all backend
/// simulation state but reset Rapier's disposable per-pipeline work buffers.
/// No parallel Rapier feature is enabled. Contact transitions are sorted by
/// stable Hycel body IDs, independent of backend handle order.
pub struct PhysicsWorld {
    config: PhysicsConfig,
    gravity: Vector,
    integration_parameters: IntegrationParameters,
    pipeline: PhysicsPipeline,
    islands: IslandManager,
    broad_phase: BroadPhaseBvh,
    narrow_phase: NarrowPhase,
    bodies: RigidBodySet,
    colliders: ColliderSet,
    impulse_joints: ImpulseJointSet,
    multibody_joints: MultibodyJointSet,
    ccd_solver: CCDSolver,
    handles: BTreeMap<BodyId, RigidBodyHandle>,
    kinds: BTreeMap<BodyId, BodyKind>,
    next_body_id: u64,
    last_tick: Option<u64>,
    active_contacts: BTreeSet<ContactPair>,
    contact_events: Vec<ContactEvent>,
}

impl Clone for PhysicsWorld {
    fn clone(&self) -> Self {
        Self {
            config: self.config,
            gravity: self.gravity,
            integration_parameters: self.integration_parameters,
            pipeline: PhysicsPipeline::default(),
            islands: self.islands.clone(),
            broad_phase: self.broad_phase.clone(),
            narrow_phase: self.narrow_phase.clone(),
            bodies: self.bodies.clone(),
            colliders: self.colliders.clone(),
            impulse_joints: self.impulse_joints.clone(),
            multibody_joints: self.multibody_joints.clone(),
            ccd_solver: self.ccd_solver.clone(),
            handles: self.handles.clone(),
            kinds: self.kinds.clone(),
            next_body_id: self.next_body_id,
            last_tick: self.last_tick,
            active_contacts: self.active_contacts.clone(),
            contact_events: self.contact_events.clone(),
        }
    }
}

impl PhysicsWorld {
    /// Creates an empty world configured for a fixed tick rate and gravity.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsError::InvalidTickRate`] for rates outside `1..=1000`
    /// or [`PhysicsError::ValueOutOfRange`] for gravity outside the adapter range.
    pub fn new(config: PhysicsConfig) -> Result<Self, PhysicsError> {
        if !(1..=MAX_TICK_RATE).contains(&config.tick_rate_hz) {
            return Err(PhysicsError::InvalidTickRate);
        }
        let gravity = backend_vector(config.gravity)?;
        let tick_rate =
            u16::try_from(config.tick_rate_hz).map_err(|_| PhysicsError::InvalidTickRate)?;
        let integration_parameters = IntegrationParameters {
            dt: 1.0 / f32::from(tick_rate),
            ..IntegrationParameters::default()
        };
        Ok(Self {
            config,
            gravity,
            integration_parameters,
            pipeline: PhysicsPipeline::default(),
            islands: IslandManager::default(),
            broad_phase: BroadPhaseBvh::default(),
            narrow_phase: NarrowPhase::default(),
            bodies: RigidBodySet::default(),
            colliders: ColliderSet::default(),
            impulse_joints: ImpulseJointSet::default(),
            multibody_joints: MultibodyJointSet::default(),
            ccd_solver: CCDSolver,
            handles: BTreeMap::new(),
            kinds: BTreeMap::new(),
            next_body_id: 1,
            last_tick: None,
            active_contacts: BTreeSet::new(),
            contact_events: Vec::new(),
        })
    }

    /// Inserts a validated box and returns its stable world-local ID.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid geometry/body-kind combinations, out-of-range
    /// values, or when [`MAX_BODIES`] is reached.
    pub fn insert_box(&mut self, description: BoxBody) -> Result<BodyId, PhysicsError> {
        if self.handles.len() >= MAX_BODIES {
            return Err(PhysicsError::BodyLimitReached);
        }
        if description.half_extents.x.milli_units() <= 0
            || description.half_extents.y.milli_units() <= 0
        {
            return Err(PhysicsError::InvalidGeometry);
        }
        if description.continuous_collision_detection && description.kind != BodyKind::Dynamic {
            return Err(PhysicsError::CcdRequiresDynamicBody);
        }
        if description.kind == BodyKind::Fixed && description.velocity != Vec2::ZERO {
            return Err(PhysicsError::FixedBodyVelocity);
        }
        let position = backend_vector(description.position)?;
        let half_extents = backend_vector(description.half_extents)?;
        let velocity = backend_vector(description.velocity)?;
        let body_id = BodyId(self.next_body_id);
        self.next_body_id = self
            .next_body_id
            .checked_add(1)
            .ok_or(PhysicsError::BodyIdsExhausted)?;
        let builder = match description.kind {
            BodyKind::Fixed => RigidBodyBuilder::fixed().translation(position),
            BodyKind::Dynamic => RigidBodyBuilder::dynamic()
                .translation(position)
                .linvel(velocity)
                .ccd_enabled(description.continuous_collision_detection),
            BodyKind::Kinematic => RigidBodyBuilder::kinematic_velocity_based()
                .translation(position)
                .linvel(velocity),
        }
        .user_data(u128::from(body_id.0));
        let handle = self.bodies.insert(builder.build());
        let collider = ColliderBuilder::cuboid(half_extents.x, half_extents.y).build();
        self.colliders
            .insert_with_parent(collider, handle, &mut self.bodies);
        self.handles.insert(body_id, handle);
        self.kinds.insert(body_id, description.kind);
        Ok(body_id)
    }

    /// Removes a body and its attached collider.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsError::UnknownBody`] when `id` is not in this world.
    pub fn remove_body(&mut self, id: BodyId) -> Result<(), PhysicsError> {
        let handle = self.handles.remove(&id).ok_or(PhysicsError::UnknownBody)?;
        self.kinds.remove(&id);
        self.bodies.remove(
            handle,
            &mut self.islands,
            &mut self.colliders,
            &mut self.impulse_joints,
            &mut self.multibody_joints,
            true,
        );
        Ok(())
    }

    /// Assigns linear velocity in world units per second.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsError::UnknownBody`] for an absent ID,
    /// [`PhysicsError::FixedBodyVelocity`] for an immovable body, or
    /// [`PhysicsError::ValueOutOfRange`] when velocity is outside the adapter range.
    pub fn set_velocity(&mut self, id: BodyId, velocity: Vec2) -> Result<(), PhysicsError> {
        if self.kinds.get(&id).ok_or(PhysicsError::UnknownBody)? == &BodyKind::Fixed {
            return Err(PhysicsError::FixedBodyVelocity);
        }
        let velocity = backend_vector(velocity)?;
        self.bodies
            .get_mut(self.handles[&id])
            .ok_or(PhysicsError::UnknownBody)?
            .set_linvel(velocity, true);
        Ok(())
    }

    /// Reads the body's fixed-point center and linear velocity.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsError::UnknownBody`] for an absent ID or
    /// [`PhysicsError::InvalidBackendState`] for a non-finite/out-of-range backend value.
    pub fn body_state(&self, id: BodyId) -> Result<BodyState, PhysicsError> {
        let handle = self.handles.get(&id).ok_or(PhysicsError::UnknownBody)?;
        let body = self.bodies.get(*handle).ok_or(PhysicsError::UnknownBody)?;
        Ok(BodyState {
            position: fixed_vector(body.translation())?,
            velocity: fixed_vector(body.linvel())?,
        })
    }

    /// IDs of current bodies in stable ascending order.
    pub fn body_ids(&self) -> impl Iterator<Item = BodyId> + '_ {
        self.handles.keys().copied()
    }

    /// Advances exactly one fixed simulation tick and returns sorted contact transitions.
    ///
    /// The first call must use tick zero. Later calls must use the prior tick plus
    /// one. A caller that owns a cloneable simulation state can use the world's
    /// `Clone` implementation to restore state after an enclosing tick failure.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsError::NonContiguousTick`] for skipped or repeated ticks,
    /// [`PhysicsError::TickExhausted`] after tick `u64::MAX`, or
    /// [`PhysicsError::InvalidBackendState`] if a resulting body state cannot be
    /// represented by the adapter's bounded fixed-point coordinates.
    pub fn step(&mut self, tick: u64) -> Result<&[ContactEvent], PhysicsError> {
        let expected = match self.last_tick {
            Some(previous) => previous.checked_add(1).ok_or(PhysicsError::TickExhausted)?,
            None => 0,
        };
        if tick != expected {
            return Err(PhysicsError::NonContiguousTick);
        }
        self.pipeline.step(
            self.gravity,
            &self.integration_parameters,
            &mut self.islands,
            &mut self.broad_phase,
            &mut self.narrow_phase,
            &mut self.bodies,
            &mut self.colliders,
            &mut self.impulse_joints,
            &mut self.multibody_joints,
            &mut self.ccd_solver,
            &(),
            &(),
        );
        let mut current_contacts = BTreeSet::new();
        for pair in self.narrow_phase.contact_pairs() {
            if !pair.has_any_active_contact() {
                continue;
            }
            let Some(first) = body_id_for_collider(&self.colliders, &self.bodies, pair.collider1)
            else {
                continue;
            };
            let Some(second) = body_id_for_collider(&self.colliders, &self.bodies, pair.collider2)
            else {
                continue;
            };
            if first != second {
                current_contacts.insert(contact_pair(first, second));
            }
        }
        self.contact_events.clear();
        for pair in current_contacts.difference(&self.active_contacts) {
            self.contact_events.push(ContactEvent {
                pair: *pair,
                kind: ContactKind::Started,
            });
        }
        for pair in self.active_contacts.difference(&current_contacts) {
            self.contact_events.push(ContactEvent {
                pair: *pair,
                kind: ContactKind::Stopped,
            });
        }
        self.contact_events.sort_unstable();
        self.active_contacts = current_contacts;
        self.last_tick = Some(tick);
        Ok(&self.contact_events)
    }

    /// Last successfully simulated tick, or `None` before the first step.
    #[must_use]
    pub const fn last_tick(&self) -> Option<u64> {
        self.last_tick
    }

    /// Fixed configuration used by this world.
    #[must_use]
    pub const fn config(&self) -> PhysicsConfig {
        self.config
    }
}

fn contact_pair(first: BodyId, second: BodyId) -> ContactPair {
    if first < second {
        ContactPair { first, second }
    } else {
        ContactPair {
            first: second,
            second: first,
        }
    }
}

fn body_id_for_collider(
    colliders: &ColliderSet,
    bodies: &RigidBodySet,
    collider: ColliderHandle,
) -> Option<BodyId> {
    let parent = colliders.get(collider)?.parent()?;
    let body = bodies.get(parent)?;
    u64::try_from(body.user_data).ok().map(BodyId)
}

fn backend_vector(value: Vec2) -> Result<Vector, PhysicsError> {
    Ok(Vector::new(
        backend_scalar(value.x)?,
        backend_scalar(value.y)?,
    ))
}

fn backend_scalar(value: SimScalar) -> Result<f32, PhysicsError> {
    let milli_units = value.milli_units();
    if milli_units.unsigned_abs() > MAX_ABS_MILLI_UNITS as u64 {
        return Err(PhysicsError::ValueOutOfRange);
    }
    // The validated bound (10,000,000) is below f32's exact-integer limit.
    #[allow(clippy::cast_precision_loss)]
    let milli_units = milli_units as f32;
    Ok(milli_units / 1_000.0)
}

fn fixed_vector(value: Vector) -> Result<Vec2, PhysicsError> {
    Ok(Vec2::new(fixed_scalar(value.x)?, fixed_scalar(value.y)?))
}

fn fixed_scalar(value: f32) -> Result<SimScalar, PhysicsError> {
    if !value.is_finite() {
        return Err(PhysicsError::InvalidBackendState);
    }
    let milli_units = (f64::from(value) * 1_000.0).round();
    if !(-10_000_000.0..=10_000_000.0).contains(&milli_units) {
        return Err(PhysicsError::InvalidBackendState);
    }
    // The preceding range check and round() guarantee this is an integral i64.
    #[allow(clippy::cast_possible_truncation)]
    let milli_units = milli_units as i64;
    Ok(SimScalar::from_milli_units(milli_units))
}

#[cfg(test)]
mod tests {
    use super::{
        BodyKind, BoxBody, ContactEvent, ContactKind, ContactPair, MAX_ABS_MILLI_UNITS,
        PhysicsConfig, PhysicsError, PhysicsWorld,
    };
    use hycel_core::{SimScalar, Vec2};

    fn units(value: i64) -> SimScalar {
        SimScalar::from_milli_units(value)
    }

    fn vector(x: i64, y: i64) -> Vec2 {
        Vec2::new(units(x), units(y))
    }

    fn box_body(kind: BodyKind, x: i64, y: i64, hx: i64, hy: i64) -> BoxBody {
        BoxBody::new(kind, vector(x, y), vector(hx, hy))
    }

    #[test]
    fn fixed_step_starts_at_zero_and_rejects_skips_and_duplicates() {
        let mut world = PhysicsWorld::new(PhysicsConfig::default()).unwrap();
        assert_eq!(world.step(1), Err(PhysicsError::NonContiguousTick));
        assert_eq!(world.last_tick(), None);
        world.step(0).unwrap();
        assert_eq!(world.last_tick(), Some(0));
        assert_eq!(world.step(0), Err(PhysicsError::NonContiguousTick));
        assert_eq!(world.step(2), Err(PhysicsError::NonContiguousTick));
        world.step(1).unwrap();
    }

    #[test]
    fn boxes_fall_and_contact_events_use_sorted_stable_body_ids() {
        let config = PhysicsConfig::new(60, Vec2::ZERO);
        let mut world = PhysicsWorld::new(config).unwrap();
        let floor = world
            .insert_box(box_body(BodyKind::Fixed, 0, 0, 2_000, 500))
            .unwrap();
        let player = world
            .insert_box(box_body(BodyKind::Dynamic, 0, -1_000, 500, 500))
            .unwrap();
        let first = world.step(0).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(
            first[0],
            ContactEvent {
                pair: ContactPair {
                    first: floor,
                    second: player,
                },
                kind: ContactKind::Started,
            }
        );
        assert_eq!(world.step(1).unwrap(), &[]);
        world.set_velocity(player, vector(300_000, 0)).unwrap();
        let mut stopped = None;
        for tick in 2..20 {
            if let Some(event) = world.step(tick).unwrap().first() {
                stopped = Some(*event);
                break;
            }
        }
        let stopped = stopped.expect("moving away from the floor ends the contact");
        assert_eq!(stopped.kind, ContactKind::Stopped);
        assert_eq!(stopped.pair.first, floor);
        assert_eq!(stopped.pair.second, player);
    }

    #[test]
    fn simultaneous_contacts_are_sorted_by_hycel_body_ids() {
        let mut world = PhysicsWorld::new(PhysicsConfig::new(60, Vec2::ZERO)).unwrap();
        let anchor = world
            .insert_box(box_body(BodyKind::Fixed, 0, 0, 2_000, 2_000))
            .unwrap();
        let first = world
            .insert_box(box_body(BodyKind::Dynamic, 0, 0, 500, 500))
            .unwrap();
        let second = world
            .insert_box(box_body(BodyKind::Dynamic, 0, 0, 500, 500))
            .unwrap();
        let events = world.step(0).unwrap();
        assert!(events.len() >= 2);
        assert!(events.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(
            events
                .iter()
                .all(|event| event.pair.first < event.pair.second)
        );
        assert!(events.iter().any(|event| event.pair
            == ContactPair {
                first: anchor,
                second: first
            }));
        assert!(events.iter().any(|event| event.pair
            == ContactPair {
                first: anchor,
                second
            }));
    }

    #[test]
    fn continuous_collision_detection_prevents_high_speed_tunneling() {
        let config = PhysicsConfig::new(60, Vec2::ZERO);
        let mut world = PhysicsWorld::new(config).unwrap();
        let wall = world
            .insert_box(box_body(BodyKind::Fixed, 0, 0, 100, 2_000))
            .unwrap();
        let bullet = world
            .insert_box(
                box_body(BodyKind::Dynamic, -2_000, 0, 100, 100)
                    .with_velocity(vector(300_000, 0))
                    .with_continuous_collision_detection(),
            )
            .unwrap();
        world.step(0).unwrap();
        let state = world.body_state(bullet).unwrap();
        assert!(state.position.x.milli_units() < 0);
        assert!(world.step(1).unwrap().iter().any(|event| {
            event.kind == ContactKind::Started
                && event.pair
                    == ContactPair {
                        first: wall,
                        second: bullet,
                    }
        }));
    }

    #[test]
    fn clone_preserves_simulation_state_for_rollback_and_replay() {
        let config = PhysicsConfig::default();
        let mut original = PhysicsWorld::new(config).unwrap();
        original
            .insert_box(box_body(BodyKind::Fixed, 0, 0, 5_000, 500))
            .unwrap();
        let falling = original
            .insert_box(box_body(BodyKind::Dynamic, 0, -5_000, 500, 500))
            .unwrap();
        for tick in 0..30 {
            original.step(tick).unwrap();
        }
        let mut restored = original.clone();
        for tick in 30..180 {
            let original_events = original.step(tick).unwrap().to_vec();
            let restored_events = restored.step(tick).unwrap().to_vec();
            assert_eq!(original_events, restored_events);
            assert_eq!(original.body_state(falling), restored.body_state(falling));
            assert_eq!(original.step(tick), Err(PhysicsError::NonContiguousTick));
        }
    }

    #[test]
    fn invalid_geometry_ranges_and_body_kinds_are_rejected() {
        let mut world = PhysicsWorld::new(PhysicsConfig::default()).unwrap();
        assert_eq!(
            world.insert_box(box_body(BodyKind::Fixed, 0, 0, 0, 500)),
            Err(PhysicsError::InvalidGeometry)
        );
        assert_eq!(
            world.insert_box(box_body(
                BodyKind::Fixed,
                MAX_ABS_MILLI_UNITS + 1,
                0,
                500,
                500
            )),
            Err(PhysicsError::ValueOutOfRange)
        );
        assert_eq!(
            world.insert_box(
                box_body(BodyKind::Fixed, 0, 0, 500, 500).with_continuous_collision_detection()
            ),
            Err(PhysicsError::CcdRequiresDynamicBody)
        );
        let fixed = world
            .insert_box(box_body(BodyKind::Fixed, 0, 0, 500, 500))
            .unwrap();
        assert_eq!(
            world.set_velocity(fixed, vector(1_000, 0)),
            Err(PhysicsError::FixedBodyVelocity)
        );
        assert_eq!(
            world.set_velocity(fixed, vector(MAX_ABS_MILLI_UNITS + 1, 0)),
            Err(PhysicsError::FixedBodyVelocity)
        );
        assert_eq!(
            world.body_state(super::BodyId(999)),
            Err(PhysicsError::UnknownBody)
        );
        assert!(PhysicsWorld::new(PhysicsConfig::new(0, Vec2::ZERO)).is_err());
        assert!(PhysicsWorld::new(PhysicsConfig::new(1_001, Vec2::ZERO)).is_err());
    }

    #[test]
    fn removing_a_touching_body_emits_a_sorted_stopped_transition() {
        let mut world = PhysicsWorld::new(PhysicsConfig::new(60, Vec2::ZERO)).unwrap();
        let floor = world
            .insert_box(box_body(BodyKind::Fixed, 0, 0, 1_000, 500))
            .unwrap();
        let body = world
            .insert_box(box_body(BodyKind::Dynamic, 0, -1_000, 500, 500))
            .unwrap();
        world.step(0).unwrap();
        world.remove_body(body).unwrap();
        let events = world.step(1).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, ContactKind::Stopped);
        assert_eq!(
            events[0].pair,
            ContactPair {
                first: floor,
                second: body
            }
        );
        assert!(world.body_ids().eq([floor]));
    }
}
