//! A generational arena: dense slots, a LIFO free list, and iteration in index order.
//! Hand-rolled so the kernel stays dependency-free and every traversal is deterministic.

/// A handle to an arena slot; stale handles (a removed and reused slot) resolve to `None`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct EntityId {
    pub index: u32,
    pub generation: u32,
}

impl EntityId {
    pub const NONE: EntityId = EntityId {
        index: u32::MAX,
        generation: u32::MAX,
    };

    pub fn is_none(self) -> bool {
        self == EntityId::NONE
    }

    pub fn is_some(self) -> bool {
        !self.is_none()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arena<T> {
    slots: Vec<Option<T>>,
    generation: Vec<u32>,
    free: Vec<u32>,
}

impl<T> Default for Arena<T> {
    fn default() -> Self {
        Arena {
            slots: Vec::new(),
            generation: Vec::new(),
            free: Vec::new(),
        }
    }
}

impl<T> Arena<T> {
    pub fn new() -> Arena<T> {
        Arena::default()
    }

    /// Reuses the most recently freed slot, else appends one.
    pub fn insert(&mut self, value: T) -> EntityId {
        if let Some(index) = self.free.pop() {
            let i = index as usize;
            debug_assert!(self.slots[i].is_none());
            self.slots[i] = Some(value);
            return EntityId {
                index,
                generation: self.generation[i],
            };
        }
        let index = self.slots.len() as u32;
        self.slots.push(Some(value));
        self.generation.push(0);
        EntityId {
            index,
            generation: 0,
        }
    }

    /// Frees the slot and bumps its generation so the old id goes stale.
    pub fn remove(&mut self, id: EntityId) -> Option<T> {
        if !self.contains(id) {
            return None;
        }
        let i = id.index as usize;
        let value = self.slots[i].take();
        self.generation[i] = self.generation[i].wrapping_add(1);
        self.free.push(id.index);
        value
    }

    pub fn contains(&self, id: EntityId) -> bool {
        let i = id.index as usize;
        i < self.slots.len() && self.generation[i] == id.generation && self.slots[i].is_some()
    }

    pub fn get(&self, id: EntityId) -> Option<&T> {
        if self.contains(id) {
            self.slots[id.index as usize].as_ref()
        } else {
            None
        }
    }

    pub fn get_mut(&mut self, id: EntityId) -> Option<&mut T> {
        if self.contains(id) {
            self.slots[id.index as usize].as_mut()
        } else {
            None
        }
    }

    pub fn len(&self) -> usize {
        self.slots.len() - self.free.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Live entries in index order.
    pub fn iter(&self) -> impl Iterator<Item = (EntityId, &T)> {
        self.slots.iter().enumerate().filter_map(|(i, s)| {
            s.as_ref().map(|v| {
                (
                    EntityId {
                        index: i as u32,
                        generation: self.generation[i],
                    },
                    v,
                )
            })
        })
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (EntityId, &mut T)> {
        let generation = &self.generation;
        self.slots.iter_mut().enumerate().filter_map(move |(i, s)| {
            s.as_mut().map(|v| {
                (
                    EntityId {
                        index: i as u32,
                        generation: generation[i],
                    },
                    v,
                )
            })
        })
    }

    /// Live ids in index order (a cheap way to iterate while mutating other state).
    pub fn ids(&self) -> Vec<EntityId> {
        self.iter().map(|(id, _)| id).collect()
    }

    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// The raw slot: its generation and its value, if live.
    pub fn slot(&self, index: usize) -> (u32, Option<&T>) {
        (self.generation[index], self.slots[index].as_ref())
    }

    pub fn free_list(&self) -> &[u32] {
        &self.free
    }

    /// Rebuilds an arena from saved parts; `None` if the parts are inconsistent.
    pub fn from_parts(
        slots: Vec<Option<T>>,
        generation: Vec<u32>,
        free: Vec<u32>,
    ) -> Option<Arena<T>> {
        if slots.len() != generation.len() {
            return None;
        }
        let mut seen = vec![false; slots.len()];
        for &f in &free {
            let i = f as usize;
            if i >= slots.len() || slots[i].is_some() || seen[i] {
                return None;
            }
            seen[i] = true;
        }
        if slots
            .iter()
            .enumerate()
            .any(|(i, s)| s.is_none() && !seen[i])
        {
            return None;
        }
        Some(Arena {
            slots,
            generation,
            free,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reuses_slots_with_new_generations_and_iterates_in_index_order() {
        let mut arena = Arena::new();
        let a = arena.insert("a");
        let b = arena.insert("b");
        let c = arena.insert("c");
        assert_eq!(arena.remove(b), Some("b"));
        assert_eq!(arena.get(b), None, "stale id");
        assert_eq!(arena.len(), 2);
        let d = arena.insert("d");
        assert_eq!(d.index, b.index);
        assert_eq!(d.generation, b.generation + 1);
        assert_eq!(arena.get(b), None);
        assert_eq!(arena.get(d), Some(&"d"));
        let order: Vec<&str> = arena.iter().map(|(_, v)| *v).collect();
        assert_eq!(order, vec!["a", "d", "c"]);
        assert_eq!(arena.ids(), vec![a, d, c]);
        assert_eq!(arena.remove(b), None);
        for (_, v) in arena.iter_mut() {
            *v = "z";
        }
        assert!(arena.iter().all(|(_, v)| *v == "z"));
    }

    #[test]
    fn parts_round_trip_and_bad_parts_are_rejected() {
        let mut arena = Arena::new();
        let a = arena.insert(1u8);
        let b = arena.insert(2u8);
        arena.insert(3u8);
        arena.remove(a);
        arena.remove(b);
        let slots: Vec<Option<u8>> = (0..arena.slot_count())
            .map(|i| arena.slot(i).1.copied())
            .collect();
        let generation: Vec<u32> = (0..arena.slot_count()).map(|i| arena.slot(i).0).collect();
        let mut rebuilt = Arena::from_parts(
            slots.clone(),
            generation.clone(),
            arena.free_list().to_vec(),
        )
        .unwrap();
        assert_eq!(rebuilt, arena);
        assert_eq!(
            rebuilt.insert(9).index,
            b.index,
            "the free list order is preserved"
        );
        assert!(
            Arena::from_parts(slots.clone(), generation.clone(), vec![0, 0]).is_none(),
            "duplicate free entry"
        );
        assert!(
            Arena::from_parts(slots.clone(), generation.clone(), vec![0]).is_none(),
            "a free slot not listed"
        );
        assert!(
            Arena::from_parts(slots, vec![0], vec![]).is_none(),
            "length mismatch"
        );
    }
}
