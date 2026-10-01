//! Deterministic entity identity and typed component storage.
//!
//! IDs are stable for an entity's lifetime and include a generation so a stale
//! handle never aliases a later entity that reuses the same slot.

use std::collections::BTreeMap;

use crate::{CanonicalState, CanonicalWriter};

/// Runtime identity for one entity. Fields are intentionally private; create IDs
/// through [`World::spawn`] and inspect them through the accessors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntityId {
    index: u32,
    generation: u32,
}

impl CanonicalState for EntityId {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.write_u32(self.index);
        writer.write_u32(self.generation);
    }
}

impl EntityId {
    /// Slot index, assigned deterministically by [`World::spawn`].
    #[must_use]
    pub const fn index(self) -> u32 {
        self.index
    }

    /// Slot generation, incremented whenever the slot is despawned and reused.
    #[must_use]
    pub const fn generation(self) -> u32 {
        self.generation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EntitySlot {
    id: EntityId,
    alive: bool,
}

/// Small deterministic entity arena. New slots are allocated in increasing
/// index order; freed slots are reused in last-despawned-first order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct World {
    slots: Vec<EntitySlot>,
    free_indices: Vec<u32>,
    alive_count: usize,
}

impl CanonicalState for World {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.write_sequence_len(self.slots.len());
        for slot in &self.slots {
            slot.id.write_canonical(writer);
            writer.write_bool(slot.alive);
        }
        writer.write_sequence_len(self.free_indices.len());
        for index in &self.free_indices {
            writer.write_u32(*index);
        }
        writer.write_sequence_len(self.alive_count);
    }
}

impl World {
    /// Creates a live entity with a deterministic ID.
    ///
    /// Reuse is deterministic: the most recently despawned reusable slot is
    /// selected first. A generation that reaches `u32::MAX` is retired rather
    /// than wrapped, so no stale ID can ever become valid again.
    ///
    /// # Errors
    ///
    /// Returns [`WorldError::CapacityExceeded`] when no new `u32` slot index is
    /// representable.
    pub fn spawn(&mut self) -> Result<EntityId, WorldError> {
        if let Some(index) = self.free_indices.pop() {
            let slot_index = usize::try_from(index).map_err(|_| WorldError::CapacityExceeded)?;
            let slot = &mut self.slots[slot_index];
            debug_assert!(!slot.alive);
            slot.alive = true;
            self.alive_count += 1;
            return Ok(slot.id);
        }

        let index = u32::try_from(self.slots.len()).map_err(|_| WorldError::CapacityExceeded)?;
        let id = EntityId {
            index,
            generation: 0,
        };
        self.slots.push(EntitySlot { id, alive: true });
        self.alive_count += 1;
        Ok(id)
    }

    /// Despawns a live entity. Despawned IDs become invalid immediately.
    ///
    /// Component stores are separate typed values; query them with the world to
    /// exclude dead entities, and call [`ComponentStorage::retain_alive`] after
    /// a batch of despawns to reclaim their stored component data.
    ///
    /// # Errors
    ///
    /// Returns [`WorldError::NotAlive`] if `entity` was never spawned, is already
    /// despawned, or refers to an older generation.
    pub fn despawn(&mut self, entity: EntityId) -> Result<(), WorldError> {
        let slot_index = usize::try_from(entity.index).map_err(|_| WorldError::NotAlive)?;
        let Some(slot) = self.slots.get_mut(slot_index) else {
            return Err(WorldError::NotAlive);
        };
        if !slot.alive || slot.id != entity {
            return Err(WorldError::NotAlive);
        }

        slot.alive = false;
        self.alive_count -= 1;
        if let Some(next_generation) = slot.id.generation.checked_add(1) {
            slot.id.generation = next_generation;
            self.free_indices.push(entity.index);
        }
        Ok(())
    }

    /// Returns whether this exact ID refers to a currently live entity.
    #[must_use]
    pub fn is_alive(&self, entity: EntityId) -> bool {
        let Ok(index) = usize::try_from(entity.index) else {
            return false;
        };
        self.slots
            .get(index)
            .is_some_and(|slot| slot.alive && slot.id == entity)
    }

    /// Number of currently live entities.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.alive_count
    }

    /// Whether there are no currently live entities.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.alive_count == 0
    }

    /// Iterates live entities in increasing slot-index order.
    pub fn entities(&self) -> impl Iterator<Item = EntityId> + '_ {
        self.slots
            .iter()
            .filter_map(|slot| slot.alive.then_some(slot.id))
    }
}

/// A typed component map with deterministic entity-ID iteration order.
///
/// Stores are independent values rather than a global ECS registry. Methods
/// that expose live components take a [`World`] and ignore stale IDs. Call
/// [`Self::retain_alive`] after despawns to reclaim their memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentStorage<T> {
    values: BTreeMap<EntityId, T>,
}

impl<T> Default for ComponentStorage<T> {
    fn default() -> Self {
        Self {
            values: BTreeMap::new(),
        }
    }
}

impl<T> ComponentStorage<T> {
    /// Emits live entries in canonical entity order. Stale entries are excluded
    /// even if the owner has not yet reclaimed their memory.
    pub fn write_live_canonical(&self, world: &World, writer: &mut CanonicalWriter)
    where
        T: CanonicalState,
    {
        writer.write_sequence_len(self.iter(world).count());
        for (entity, component) in self.iter(world) {
            entity.write_canonical(writer);
            component.write_canonical(writer);
        }
    }

    /// Adds or replaces a component on a live entity.
    ///
    /// Returns the prior component when replacing it.
    ///
    /// # Errors
    ///
    /// Returns [`WorldError::NotAlive`] when `entity` is not live in `world`.
    pub fn insert(
        &mut self,
        world: &World,
        entity: EntityId,
        component: T,
    ) -> Result<Option<T>, WorldError> {
        if !world.is_alive(entity) {
            return Err(WorldError::NotAlive);
        }
        Ok(self.values.insert(entity, component))
    }

    /// Returns a component only when its entity is currently live.
    #[must_use]
    pub fn get(&self, world: &World, entity: EntityId) -> Option<&T> {
        if world.is_alive(entity) {
            self.values.get(&entity)
        } else {
            None
        }
    }

    /// Removes a component by exact generational ID, whether the entity is alive
    /// or stale. This is useful for explicit cleanup after despawning.
    pub fn remove(&mut self, entity: EntityId) -> Option<T> {
        self.values.remove(&entity)
    }

    /// Removes components whose entity IDs are no longer live and returns the
    /// number removed.
    pub fn retain_alive(&mut self, world: &World) -> usize {
        let previous_len = self.values.len();
        self.values.retain(|entity, _| world.is_alive(*entity));
        previous_len - self.values.len()
    }

    /// Number of stored components, including stale entries not yet reclaimed.
    #[must_use]
    pub fn stored_len(&self) -> usize {
        self.values.len()
    }

    /// Iterates live components in deterministic entity-ID order.
    pub fn iter<'a>(&'a self, world: &'a World) -> impl Iterator<Item = (EntityId, &'a T)> + 'a {
        self.values
            .iter()
            .filter(|(entity, _)| world.is_alive(**entity))
            .map(|(entity, component)| (*entity, component))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorldError {
    CapacityExceeded,
    NotAlive,
}

impl std::fmt::Display for WorldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CapacityExceeded => f.write_str("entity ID capacity has been exhausted"),
            Self::NotAlive => f.write_str("entity ID is not currently alive in this world"),
        }
    }
}

impl std::error::Error for WorldError {}

#[cfg(test)]
mod tests {
    use super::{ComponentStorage, World, WorldError};
    use crate::{CanonicalState, CanonicalWriter};

    #[derive(Clone)]
    struct HashableWorld {
        world: World,
        health: ComponentStorage<i64>,
    }

    impl CanonicalState for HashableWorld {
        fn write_canonical(&self, writer: &mut CanonicalWriter) {
            self.world.write_canonical(writer);
            self.health.write_live_canonical(&self.world, writer);
        }
    }

    #[test]
    fn live_component_hash_ignores_unreclaimed_stale_entries() {
        let mut world = World::default();
        let entity = world.spawn().unwrap();
        let mut health = ComponentStorage::default();
        health.insert(&world, entity, 100_i64).unwrap();
        let state = HashableWorld { world, health };
        let mut after_despawn = state.clone();
        after_despawn.world.despawn(entity).unwrap();
        let mut after_cleanup = after_despawn.clone();
        after_cleanup.health.retain_alive(&after_cleanup.world);

        assert_eq!(after_despawn.state_hash(), after_cleanup.state_hash());
        assert_ne!(state.state_hash(), after_despawn.state_hash());
    }

    #[test]
    fn spawn_and_despawn_are_deterministic_and_reuse_increments_generation() {
        let mut world = World::default();
        let first = world.spawn().unwrap();
        let second = world.spawn().unwrap();
        assert_eq!((first.index(), first.generation()), (0, 0));
        assert_eq!((second.index(), second.generation()), (1, 0));
        assert_eq!(world.entities().collect::<Vec<_>>(), [first, second]);

        world.despawn(first).unwrap();
        let replacement = world.spawn().unwrap();
        assert_eq!(replacement.index(), first.index());
        assert_eq!(replacement.generation(), first.generation() + 1);
        assert!(!world.is_alive(first));
        assert!(world.is_alive(replacement));
        assert_eq!(world.len(), 2);
    }

    #[test]
    fn stale_or_duplicate_despawn_is_rejected() {
        let mut world = World::default();
        let entity = world.spawn().unwrap();
        world.despawn(entity).unwrap();
        assert_eq!(world.despawn(entity), Err(WorldError::NotAlive));
        assert_eq!(world.len(), 0);
    }

    #[test]
    fn typed_components_require_live_entities_and_iterate_deterministically() {
        let mut world = World::default();
        let first = world.spawn().unwrap();
        let second = world.spawn().unwrap();
        let mut positions = ComponentStorage::default();
        positions.insert(&world, second, 20).unwrap();
        positions.insert(&world, first, 10).unwrap();

        assert_eq!(positions.get(&world, first), Some(&10));
        assert_eq!(
            positions
                .iter(&world)
                .map(|(entity, _)| entity)
                .collect::<Vec<_>>(),
            [first, second]
        );
    }

    #[test]
    fn despawned_component_is_hidden_and_can_be_reclaimed() {
        let mut world = World::default();
        let entity = world.spawn().unwrap();
        let mut health = ComponentStorage::default();
        health.insert(&world, entity, 100).unwrap();
        world.despawn(entity).unwrap();

        assert_eq!(health.get(&world, entity), None);
        assert_eq!(health.iter(&world).count(), 0);
        assert_eq!(health.stored_len(), 1);
        assert_eq!(health.retain_alive(&world), 1);
        assert_eq!(health.stored_len(), 0);
        assert_eq!(health.insert(&world, entity, 50), Err(WorldError::NotAlive));
    }
}
