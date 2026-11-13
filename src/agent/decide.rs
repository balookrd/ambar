//! The decision at the heart of the agent: given a volume name, what the API
//! server says about it and what the node holds locally, what should the node
//! do. Pure: no API calls, no I/O, no commands.
//!
//! Data-safety rule (R10): `Task::DeleteReplica` is returned only when the
//! object exists, is being deleted, this node is a `Replica` in it and this
//! node's finalizer is still on it. `Task::CleanDiskless` is never returned
//! for a node that has a backing file. Anything the table does not cover is
//! `Refuse` or `Ignore`.

use crate::crd::{NodeKind, ReplicatedVolumeSpec, to_volume_spec};
use crate::membership::replica_finalizer;
use crate::model::{InitialSync, VolumeSpec};

/// What the node holds for a volume name, read without running drbdadm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Local {
    /// The backing file exists.
    pub image: bool,
    /// A resource file, an applied copy, a device link or a configured kernel resource exists.
    pub traces: bool,
    /// The synced marker exists.
    pub synced: bool,
}

/// The object as the agent sees it; `None` when no object has this name.
pub struct ObjectView<'a> {
    pub spec: &'a ReplicatedVolumeSpec,
    /// `metadata.deletionTimestamp` is set.
    pub deleting: bool,
    pub finalizers: &'a [String],
    /// `membership::collisions`.
    pub collides_with: &'a [String],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Task {
    /// Nothing on this node concerns the volume.
    Ignore,
    /// Run Reconciler::apply with this description, then report status.
    Apply { spec: VolumeSpec, mark_done: bool },
    /// Delete the local copy (Reconciler::remove), then drop this node's finalizer.
    DeleteReplica,
    /// The local copy is already gone: drop this node's finalizer.
    DropFinalizer,
    /// Remove what a node without a disk leaves behind (Reconciler::remove on a node with no image).
    CleanDiskless,
    /// Do nothing and report: the state needs a human.
    Refuse(Refusal),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// A backing file exists for a volume that has no object.
    OrphanImage,
    /// A backing file exists but this node is not a Replica of the object.
    ImageWithoutMembership,
    /// The object is being deleted, this node is a Replica, but its finalizer is missing.
    DeletingWithoutFinalizer,
    Collision(Vec<String>),
    /// Error text of `CrdError`.
    InvalidSpec(String),
}

/// `CleanDiskless` when something is left, `Ignore` otherwise. Never reached
/// with an image: every caller has already handled that case.
fn clean_or_ignore(local: Local) -> Task {
    debug_assert!(!local.image);
    if local.traces && !local.image {
        Task::CleanDiskless
    } else {
        Task::Ignore
    }
}

/// Table rows are evaluated top to bottom, the first match wins; the comments
/// name the rows of the decision table of Task 7.
pub fn decide(node: &str, volume: &str, object: Option<&ObjectView>, local: Local) -> Task {
    // Object: none.
    let Some(object) = object else {
        // none / any / image -> Refuse(OrphanImage)
        if local.image {
            return Task::Refuse(Refusal::OrphanImage);
        }
        // none / any / traces, no image -> CleanDiskless; none / any / nothing -> Ignore
        return clean_or_ignore(local);
    };

    let kind = object.spec.nodes.iter().find(|n| n.name == node).map(|n| n.kind);

    // Object: being deleted.
    if object.deleting {
        if kind == Some(NodeKind::Replica) {
            let has_finalizer = object.finalizers.contains(&replica_finalizer(node));
            if has_finalizer {
                // deleting / Replica, finalizer / image or traces -> DeleteReplica
                if local.image || local.traces {
                    return Task::DeleteReplica;
                }
                // deleting / Replica, finalizer / nothing -> DropFinalizer
                return Task::DropFinalizer;
            }
            // deleting / Replica, no finalizer / image -> Refuse(DeletingWithoutFinalizer)
            if local.image {
                return Task::Refuse(Refusal::DeletingWithoutFinalizer);
            }
            // deleting / Replica, no finalizer / no image -> CleanDiskless if traces, else Ignore
            return clean_or_ignore(local);
        }
        // deleting / Witness, Client or not a member / image -> Refuse(ImageWithoutMembership)
        if local.image {
            return Task::Refuse(Refusal::ImageWithoutMembership);
        }
        // deleting / Witness, Client or not a member / no image -> CleanDiskless if traces, else Ignore
        return clean_or_ignore(local);
    }

    // Object: exists, not being deleted.
    match kind {
        None => {
            // exists / not a member / image -> Refuse(ImageWithoutMembership)
            if local.image {
                return Task::Refuse(Refusal::ImageWithoutMembership);
            }
            // exists / not a member / traces, no image -> CleanDiskless; nothing -> Ignore
            return clean_or_ignore(local);
        }
        // exists / Witness or Client / image -> Refuse(ImageWithoutMembership)
        Some(NodeKind::Witness | NodeKind::Client) if local.image => {
            return Task::Refuse(Refusal::ImageWithoutMembership);
        }
        Some(_) => {}
    }

    // exists / member / collides_with not empty -> Refuse(Collision)
    if !object.collides_with.is_empty() {
        return Task::Refuse(Refusal::Collision(object.collides_with.to_vec()));
    }

    // exists / member / to_volume_spec failed -> Refuse(InvalidSpec)
    let spec = match to_volume_spec(volume, object.spec) {
        Ok(spec) => spec,
        Err(err) => return Task::Refuse(Refusal::InvalidSpec(err.to_string())),
    };

    // exists / member / - -> Apply
    let mark_done = spec.initial_sync == InitialSync::Skip
        && local.synced
        && spec.initial_sync_leader().is_some_and(|leader| leader.name == node);
    Task::Apply { spec, mark_done }
}

#[cfg(test)]
#[path = "tests/decide.rs"]
mod tests;
