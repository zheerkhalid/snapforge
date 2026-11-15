//! Document state and an unlimited undo/redo stack for both object and pixel edits.
//!
//! Object edits are stored as compact diffs ([`ObjectsDiff`]): the id order before and after,
//! plus only the objects that were added, removed or changed. Pixel edits (crop, resize,
//! effects on the base image) store the before/after base images as shared `Arc`s.
//!
//! Typical editor usage:
//! ```ignore
//! let before = doc.objects.clone();          // or keep a snapshot from drag start
//! edit::move_objects(&mut doc.objects, &sel, d);
//! history.record_objects(&before, &doc.objects);
//! ```

use super::model::{Object, ObjectId};
use crate::common::Image;
use std::collections::HashMap;
use std::sync::Arc;

/// An annotated image: the base pixels plus the annotation objects (z-order = list order).
#[derive(Debug, Clone)]
pub struct Document {
    pub base: Arc<Image>,
    pub objects: Vec<Object>,
    /// Incremented whenever `base` changes (cache key for [`super::RasterCache`]).
    pub base_rev: u64,
}

impl Document {
    pub fn new(base: Image) -> Self {
        Self { base: Arc::new(base), objects: Vec::new(), base_rev: 1 }
    }

    /// Replace the base image (bumps `base_rev`).
    pub fn set_base(&mut self, base: Arc<Image>) {
        self.base = base;
        self.base_rev = self.base_rev.wrapping_add(1);
    }
}

/// Difference between two object lists.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectsDiff {
    before_order: Vec<ObjectId>,
    after_order: Vec<ObjectId>,
    /// Objects in `before` that were removed or modified.
    before_changed: Vec<Object>,
    /// Objects in `after` that were added or modified.
    after_changed: Vec<Object>,
}

impl ObjectsDiff {
    /// `None` when the lists are identical.
    pub fn between(before: &[Object], after: &[Object]) -> Option<Self> {
        let before_map: HashMap<ObjectId, &Object> = before.iter().map(|o| (o.id, o)).collect();
        let after_map: HashMap<ObjectId, &Object> = after.iter().map(|o| (o.id, o)).collect();
        let before_changed: Vec<Object> =
            before.iter().filter(|o| after_map.get(&o.id).map_or(true, |a| *a != *o)).cloned().collect();
        let after_changed: Vec<Object> =
            after.iter().filter(|o| before_map.get(&o.id).map_or(true, |b| *b != *o)).cloned().collect();
        let before_order: Vec<ObjectId> = before.iter().map(|o| o.id).collect();
        let after_order: Vec<ObjectId> = after.iter().map(|o| o.id).collect();
        if before_changed.is_empty() && after_changed.is_empty() && before_order == after_order {
            return None;
        }
        Some(Self { before_order, after_order, before_changed, after_changed })
    }

    fn rebuild(current: &mut Vec<Object>, order: &[ObjectId], changed: &[Object]) {
        let mut pool: HashMap<ObjectId, Object> = current.drain(..).map(|o| (o.id, o)).collect();
        for o in changed {
            pool.insert(o.id, o.clone());
        }
        *current = order.iter().filter_map(|id| pool.remove(id)).collect();
    }

    pub fn undo(&self, objects: &mut Vec<Object>) {
        Self::rebuild(objects, &self.before_order, &self.before_changed);
    }

    pub fn redo(&self, objects: &mut Vec<Object>) {
        Self::rebuild(objects, &self.after_order, &self.after_changed);
    }
}

/// One undoable step.
#[derive(Debug, Clone)]
pub enum Edit {
    Objects(ObjectsDiff),
    /// The base image was replaced (pixel edit). The editor keeps pixel edits in its own
    /// history (DECISIONS #86); these two are exercised by the tests only.
    #[cfg(test)]
    Base {
        before: Arc<Image>,
        after: Arc<Image>,
    },
    /// Several edits as one step (e.g. a crop that also shifts annotations).
    #[cfg(test)]
    Batch(Vec<Edit>),
}

impl Edit {
    pub fn objects(before: &[Object], after: &[Object]) -> Option<Edit> {
        ObjectsDiff::between(before, after).map(Edit::Objects)
    }

    fn undo(&self, doc: &mut Document) {
        match self {
            Edit::Objects(d) => d.undo(&mut doc.objects),
            #[cfg(test)]
            Edit::Base { before, .. } => doc.set_base(before.clone()),
            #[cfg(test)]
            Edit::Batch(v) => v.iter().rev().for_each(|e| e.undo(doc)),
        }
    }

    fn redo(&self, doc: &mut Document) {
        match self {
            Edit::Objects(d) => d.redo(&mut doc.objects),
            #[cfg(test)]
            Edit::Base { after, .. } => doc.set_base(after.clone()),
            #[cfg(test)]
            Edit::Batch(v) => v.iter().for_each(|e| e.redo(doc)),
        }
    }
}

/// Unlimited undo/redo. Edits are recorded *after* they have been applied to the document.
#[derive(Debug, Default)]
pub struct History {
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    /// Undo depth at the last save (`None` = the saved state is no longer reachable). The
    /// editor tracks "dirty" per document (DECISIONS #65); this copy is kept for the tests.
    #[cfg(test)]
    saved_at: Option<usize>,
}

impl History {
    pub fn new() -> Self {
        Self {
            #[cfg(test)]
            saved_at: Some(0),
            ..Default::default()
        }
    }

    /// Record an already-applied edit. Clears the redo stack.
    pub fn record(&mut self, edit: Edit) {
        #[cfg(test)]
        if let Some(s) = self.saved_at {
            if s > self.undo.len() {
                self.saved_at = None;
            }
        }
        self.undo.push(edit);
        self.redo.clear();
    }

    /// Record an object-list change; returns false (and records nothing) if nothing changed.
    pub fn record_objects(&mut self, before: &[Object], after: &[Object]) -> bool {
        match Edit::objects(before, after) {
            Some(e) => {
                self.record(e);
                true
            }
            None => false,
        }
    }

    /// Record a base-image change.
    #[cfg(test)]
    pub fn record_base(&mut self, before: Arc<Image>, after: Arc<Image>) {
        self.record(Edit::Base { before, after });
    }

    #[cfg(test)]
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    #[cfg(test)]
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    pub fn undo(&mut self, doc: &mut Document) -> bool {
        let Some(e) = self.undo.pop() else { return false };
        e.undo(doc);
        self.redo.push(e);
        true
    }

    pub fn redo(&mut self, doc: &mut Document) -> bool {
        let Some(e) = self.redo.pop() else { return false };
        e.redo(doc);
        self.undo.push(e);
        true
    }

    /// Remember the current state as saved.
    #[cfg(test)]
    pub fn mark_saved(&mut self) {
        self.saved_at = Some(self.undo.len());
    }

    /// True if the document differs from the last saved state.
    #[cfg(test)]
    pub fn is_dirty(&self) -> bool {
        self.saved_at != Some(self.undo.len())
    }
}
