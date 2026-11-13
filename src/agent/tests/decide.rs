use serde_json::{Value, json};

use super::*;

const NODE: &str = "n1";
const VOLUME: &str = "pvc-1";

const NONE: Local = Local {
    image: false,
    traces: false,
    synced: false,
};
const IMAGE: Local = Local {
    image: true,
    traces: true,
    synced: false,
};
const TRACES: Local = Local {
    image: false,
    traces: true,
    synced: false,
};

/// Two Replicas `r1` (id 0) and `r2` (id 1) are always there; `n1` joins with
/// `kind` (id 5), or is absent.
fn spec_of(kind: Option<&str>, initial_sync: &str, n1_address: &str) -> ReplicatedVolumeSpec {
    let mut nodes: Vec<Value> = vec![
        json!({"name": "r1", "nodeId": 0, "address": "192.0.2.1", "kind": "Replica"}),
        json!({"name": "r2", "nodeId": 1, "address": "192.0.2.2", "kind": "Replica"}),
    ];
    if let Some(kind) = kind {
        nodes.push(json!({"name": NODE, "nodeId": 5, "address": n1_address, "kind": kind}));
    }
    serde_json::from_value(json!({
        "sizeBytes": 1073741824u64,
        "protocol": "C",
        "minor": 1000,
        "port": 7900,
        "maxPeers": 7,
        "initialSync": initial_sync,
        "nodes": nodes,
    }))
    .unwrap()
}

fn spec(kind: Option<&str>) -> ReplicatedVolumeSpec {
    spec_of(kind, "Done", "192.0.2.5")
}

fn own_finalizer() -> Vec<String> {
    vec![replica_finalizer(NODE)]
}

fn view<'a>(spec: &'a ReplicatedVolumeSpec, deleting: bool, finalizers: &'a [String]) -> ObjectView<'a> {
    ObjectView {
        spec,
        deleting,
        finalizers,
        collides_with: &[],
    }
}

fn run(spec: &ReplicatedVolumeSpec, deleting: bool, finalizers: &[String], local: Local) -> Task {
    decide(NODE, VOLUME, Some(&view(spec, deleting, finalizers)), local)
}

// --- object: none -----------------------------------------------------------

#[test]
fn row_no_object_image_is_an_orphan() {
    assert_eq!(decide(NODE, VOLUME, None, IMAGE), Task::Refuse(Refusal::OrphanImage));
}

#[test]
fn row_no_object_traces_without_image_are_cleaned() {
    assert_eq!(decide(NODE, VOLUME, None, TRACES), Task::CleanDiskless);
}

#[test]
fn row_no_object_nothing_is_ignored() {
    assert_eq!(decide(NODE, VOLUME, None, NONE), Task::Ignore);
}

// --- object: being deleted --------------------------------------------------

#[test]
fn row_deleting_replica_with_finalizer_and_image_deletes() {
    assert_eq!(
        run(&spec(Some("Replica")), true, &own_finalizer(), IMAGE),
        Task::DeleteReplica
    );
}

#[test]
fn row_deleting_replica_with_finalizer_and_traces_only_deletes() {
    assert_eq!(
        run(&spec(Some("Replica")), true, &own_finalizer(), TRACES),
        Task::DeleteReplica
    );
}

#[test]
fn row_deleting_replica_with_finalizer_and_nothing_drops_the_finalizer() {
    assert_eq!(
        run(&spec(Some("Replica")), true, &own_finalizer(), NONE),
        Task::DropFinalizer
    );
}

#[test]
fn row_deleting_replica_without_finalizer_and_image_refuses() {
    let other = vec![replica_finalizer("r1")];
    assert_eq!(
        run(&spec(Some("Replica")), true, &other, IMAGE),
        Task::Refuse(Refusal::DeletingWithoutFinalizer)
    );
    assert_eq!(
        run(&spec(Some("Replica")), true, &[], IMAGE),
        Task::Refuse(Refusal::DeletingWithoutFinalizer)
    );
}

#[test]
fn row_deleting_replica_without_finalizer_and_no_image_cleans_or_ignores() {
    let s = spec(Some("Replica"));
    assert_eq!(run(&s, true, &[], TRACES), Task::CleanDiskless);
    assert_eq!(run(&s, true, &[], NONE), Task::Ignore);
}

#[test]
fn row_deleting_non_replica_with_image_refuses() {
    for kind in [Some("Witness"), Some("Client"), None] {
        let s = spec(kind);
        // Even with a replica finalizer of this node: it is not a Replica.
        for finalizers in [own_finalizer(), vec![]] {
            assert_eq!(
                run(&s, true, &finalizers, IMAGE),
                Task::Refuse(Refusal::ImageWithoutMembership),
                "{kind:?}"
            );
        }
    }
}

#[test]
fn row_deleting_non_replica_without_image_cleans_or_ignores() {
    for kind in [Some("Witness"), Some("Client"), None] {
        let s = spec(kind);
        for finalizers in [own_finalizer(), vec![]] {
            assert_eq!(run(&s, true, &finalizers, TRACES), Task::CleanDiskless, "{kind:?}");
            assert_eq!(run(&s, true, &finalizers, NONE), Task::Ignore, "{kind:?}");
        }
    }
}

// --- object: exists ---------------------------------------------------------

#[test]
fn row_exists_not_a_member_image_refuses() {
    assert_eq!(
        run(&spec(None), false, &[], IMAGE),
        Task::Refuse(Refusal::ImageWithoutMembership)
    );
}

#[test]
fn row_exists_not_a_member_traces_without_image_are_cleaned() {
    assert_eq!(run(&spec(None), false, &[], TRACES), Task::CleanDiskless);
}

#[test]
fn row_exists_not_a_member_nothing_is_ignored() {
    assert_eq!(run(&spec(None), false, &[], NONE), Task::Ignore);
}

#[test]
fn row_exists_witness_or_client_with_image_refuses() {
    for kind in ["Witness", "Client"] {
        assert_eq!(
            run(&spec(Some(kind)), false, &[], IMAGE),
            Task::Refuse(Refusal::ImageWithoutMembership),
            "{kind}"
        );
    }
}

#[test]
fn witness_or_client_without_image_applies_even_with_traces() {
    for kind in ["Witness", "Client"] {
        for local in [NONE, TRACES] {
            assert!(
                matches!(run(&spec(Some(kind)), false, &[], local), Task::Apply { .. }),
                "{kind}"
            );
        }
    }
}

#[test]
fn row_exists_collision_refuses_with_the_names() {
    let names = vec!["pvc-2".to_string(), "pvc-3".to_string()];
    for kind in ["Replica", "Witness", "Client"] {
        let s = spec(Some(kind));
        let object = ObjectView {
            spec: &s,
            deleting: false,
            finalizers: &[],
            collides_with: &names,
        };
        assert_eq!(
            decide(NODE, VOLUME, Some(&object), NONE),
            Task::Refuse(Refusal::Collision(names.clone())),
            "{kind}"
        );
    }
    // A Replica with an image still gets the collision refusal, not Apply.
    let s = spec(Some("Replica"));
    let object = ObjectView {
        spec: &s,
        deleting: false,
        finalizers: &[],
        collides_with: &names,
    };
    assert_eq!(
        decide(NODE, VOLUME, Some(&object), IMAGE),
        Task::Refuse(Refusal::Collision(names))
    );
}

#[test]
fn row_exists_invalid_spec_refuses_with_the_error_text() {
    let s = spec_of(Some("Replica"), "Done", "not-an-ip");
    let Task::Refuse(Refusal::InvalidSpec(text)) = run(&s, false, &[], NONE) else {
        panic!("expected InvalidSpec");
    };
    assert!(text.contains("not-an-ip"), "{text}");
}

#[test]
fn collision_is_checked_before_the_spec_is_converted() {
    let s = spec_of(Some("Replica"), "Done", "not-an-ip");
    let names = vec!["pvc-2".to_string()];
    let object = ObjectView {
        spec: &s,
        deleting: false,
        finalizers: &[],
        collides_with: &names,
    };
    assert_eq!(
        decide(NODE, VOLUME, Some(&object), NONE),
        Task::Refuse(Refusal::Collision(names))
    );
}

#[test]
fn row_exists_member_applies_the_converted_spec() {
    let s = spec(Some("Replica"));
    let Task::Apply { spec, mark_done } = run(&s, false, &own_finalizer(), NONE) else {
        panic!("expected Apply");
    };
    assert_eq!(spec.name, VOLUME);
    assert_eq!(spec.nodes.len(), 3);
    assert!(!mark_done);
}

// --- mark_done ---------------------------------------------------------------

/// The leader is the diskful node with the lowest node id.
fn leader_decide(node: &str, initial_sync: &str, synced: bool) -> Task {
    let s: ReplicatedVolumeSpec = serde_json::from_value(json!({
        "sizeBytes": 1073741824u64, "protocol": "C", "minor": 1000, "port": 7900, "maxPeers": 2,
        "initialSync": initial_sync,
        "nodes": [
            {"name": "n1", "nodeId": 0, "address": "192.0.2.1", "kind": "Replica"},
            {"name": "n2", "nodeId": 1, "address": "192.0.2.2", "kind": "Replica"},
            {"name": "w", "nodeId": 2, "address": "192.0.2.3", "kind": "Witness"},
        ],
    }))
    .unwrap();
    let local = Local {
        image: node != "w",
        traces: true,
        synced,
    };
    let object = ObjectView {
        spec: &s,
        deleting: false,
        finalizers: &[],
        collides_with: &[],
    };
    decide(node, VOLUME, Some(&object), local)
}

fn mark_done_of(task: Task) -> bool {
    match task {
        Task::Apply { mark_done, .. } => mark_done,
        other => panic!("expected Apply, got {other:?}"),
    }
}

#[test]
fn mark_done_for_the_skip_leader_with_the_marker() {
    assert!(mark_done_of(leader_decide("n1", "Skip", true)));
}

#[test]
fn mark_done_false_for_a_node_that_is_not_the_leader() {
    assert!(!mark_done_of(leader_decide("n2", "Skip", true)));
    assert!(!mark_done_of(leader_decide("w", "Skip", true)));
}

#[test]
fn mark_done_false_when_initial_sync_is_done() {
    assert!(!mark_done_of(leader_decide("n1", "Done", true)));
}

#[test]
fn mark_done_false_without_the_synced_marker() {
    assert!(!mark_done_of(leader_decide("n1", "Skip", false)));
}

// --- invariants over every combination ---------------------------------------

#[test]
fn exhaustive_invariants() {
    let mut checked = 0usize;
    for object_state in ["none", "exists", "deleting"] {
        for kind in [None, Some("Replica"), Some("Witness"), Some("Client")] {
            for own_fin in [false, true] {
                for image in [false, true] {
                    for traces in [false, true] {
                        for synced in [false, true] {
                            for collision in [false, true] {
                                for initial_sync in ["Skip", "Done"] {
                                    for address in ["192.0.2.5", "bogus"] {
                                        let s = spec_of(kind, initial_sync, address);
                                        let finalizers = if own_fin {
                                            vec![replica_finalizer("r1"), replica_finalizer(NODE)]
                                        } else {
                                            vec![replica_finalizer("r1")]
                                        };
                                        let names = if collision { vec!["pvc-2".to_string()] } else { vec![] };
                                        let object = ObjectView {
                                            spec: &s,
                                            deleting: object_state == "deleting",
                                            finalizers: &finalizers,
                                            collides_with: &names,
                                        };
                                        let local = Local { image, traces, synced };
                                        let task =
                                            decide(NODE, VOLUME, (object_state != "none").then_some(&object), local);
                                        let ctx = format!(
                                            "{object_state} {kind:?} fin={own_fin} {local:?} coll={collision} {initial_sync} {address} -> {task:?}"
                                        );
                                        let may_delete =
                                            object_state == "deleting" && kind == Some("Replica") && own_fin;
                                        // R10.
                                        if task == Task::DeleteReplica {
                                            assert!(may_delete, "{ctx}");
                                        }
                                        if may_delete && (image || traces) {
                                            assert_eq!(task, Task::DeleteReplica, "{ctx}");
                                        }
                                        if task == Task::DropFinalizer {
                                            assert!(may_delete && !image && !traces, "{ctx}");
                                        }
                                        // Never clean a node that has a backing file.
                                        if image {
                                            assert_ne!(task, Task::CleanDiskless, "{ctx}");
                                        }
                                        // Apply only for a live object the node is a member of.
                                        if matches!(task, Task::Apply { .. }) {
                                            assert!(object_state == "exists" && kind.is_some(), "{ctx}");
                                            assert!(!collision && address != "bogus", "{ctx}");
                                            assert!(!image || kind == Some("Replica"), "{ctx}");
                                        }
                                        // An image is never left without an answer other than
                                        // delete, apply or refuse.
                                        if image {
                                            assert!(
                                                matches!(
                                                    task,
                                                    Task::DeleteReplica | Task::Apply { .. } | Task::Refuse(_)
                                                ),
                                                "{ctx}"
                                            );
                                        }
                                        checked += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(checked, 3 * 4 * 2 * 2 * 2 * 2 * 2 * 2 * 2);
}
