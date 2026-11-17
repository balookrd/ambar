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
        // Without this node's finalizer there is nothing to drop.
        assert_eq!(run(&s, true, &[], TRACES), Task::CleanDiskless, "{kind:?}");
        assert_eq!(run(&s, true, &[], NONE), Task::Ignore, "{kind:?}");
        // With it, traces are cleaned first: the finalizer goes in a later pass.
        assert_eq!(run(&s, true, &own_finalizer(), TRACES), Task::CleanDiskless, "{kind:?}");
    }
}

#[test]
fn row_deleting_non_replica_with_a_stray_finalizer_and_nothing_local_drops_it() {
    // The object waits for a finalizer nobody else will remove.
    for kind in [Some("Witness"), Some("Client"), None] {
        assert_eq!(
            run(&spec(kind), true, &own_finalizer(), NONE),
            Task::DropFinalizer,
            "{kind:?}"
        );
    }
}

#[test]
fn a_stray_finalizer_with_a_backing_file_is_never_dropped() {
    for kind in [Some("Witness"), Some("Client"), None] {
        assert_eq!(
            run(&spec(kind), true, &own_finalizer(), IMAGE),
            Task::Refuse(Refusal::ImageWithoutMembership),
            "{kind:?}"
        );
    }
}

// --- node listed more than once -----------------------------------------------

/// `spec(kind)` with `n1` listed a second time (another id and address).
fn spec_with_duplicate(kind: &str) -> ReplicatedVolumeSpec {
    let mut s = spec(Some(kind));
    let mut again = s.nodes.last().unwrap().clone();
    again.node_id = 6;
    again.address = "192.0.2.6".to_string();
    s.nodes.push(again);
    s
}

#[test]
fn a_node_listed_twice_is_refused_in_every_state() {
    for kind in ["Replica", "Witness", "Client"] {
        let s = spec_with_duplicate(kind);
        for deleting in [false, true] {
            for finalizers in [own_finalizer(), vec![]] {
                for local in [NONE, TRACES, IMAGE] {
                    let task = run(&s, deleting, &finalizers, local);
                    assert!(
                        matches!(task, Task::Refuse(Refusal::InvalidSpec(ref text)) if text.contains(NODE)),
                        "{kind} deleting={deleting} {local:?} -> {task:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn a_duplicate_of_another_node_does_not_concern_this_one() {
    let mut s = spec(Some("Replica"));
    let mut again = s.nodes[0].clone();
    again.node_id = 6;
    again.address = "192.0.2.6".to_string();
    s.nodes.push(again);
    assert_eq!(
        run(&s, true, &own_finalizer(), IMAGE),
        Task::DeleteReplica,
        "a duplicate of r1 is the spec validation's business, not a reason to stop deleting n1"
    );
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

// --- retiring and replica finalizer checks (Task 5) -------------------------

#[test]
fn deleting_retiring_behaves_like_deleting_replica() {
    let s = spec(Some("Retiring"));
    assert_eq!(run(&s, true, &own_finalizer(), IMAGE), Task::DeleteReplica);
    assert_eq!(run(&s, true, &own_finalizer(), TRACES), Task::DeleteReplica);
    assert_eq!(run(&s, true, &own_finalizer(), NONE), Task::DropFinalizer);

    assert_eq!(
        run(&s, true, &[], IMAGE),
        Task::Refuse(Refusal::DeletingWithoutFinalizer)
    );
    assert_eq!(run(&s, true, &[], TRACES), Task::CleanDiskless);
    assert_eq!(run(&s, true, &[], NONE), Task::Ignore);
}

#[test]
fn exists_retiring_with_finalizer_and_image_or_traces_retires() {
    let s = spec(Some("Retiring"));
    assert_eq!(run(&s, false, &own_finalizer(), IMAGE), Task::RetireReplica);
    assert_eq!(run(&s, false, &own_finalizer(), TRACES), Task::RetireReplica);
}

#[test]
fn exists_retiring_with_nothing_finishes_retire() {
    let s = spec(Some("Retiring"));
    assert_eq!(run(&s, false, &own_finalizer(), NONE), Task::FinishRetire);
    assert_eq!(run(&s, false, &[], NONE), Task::FinishRetire);
}

#[test]
fn exists_retiring_without_finalizer_and_image_or_traces_refuses() {
    let s = spec(Some("Retiring"));
    assert_eq!(
        run(&s, false, &[], IMAGE),
        Task::Refuse(Refusal::RetiringWithoutFinalizer)
    );
    assert_eq!(
        run(&s, false, &[], TRACES),
        Task::Refuse(Refusal::RetiringWithoutFinalizer)
    );
}

#[test]
fn exists_replica_without_finalizer_and_no_image_refuses() {
    let s = spec(Some("Replica"));
    assert_eq!(
        run(&s, false, &[], NONE),
        Task::Refuse(Refusal::ReplicaWithoutFinalizer)
    );
    assert_eq!(
        run(&s, false, &[], TRACES),
        Task::Refuse(Refusal::ReplicaWithoutFinalizer)
    );
}

#[test]
fn exists_replica_without_finalizer_and_image_applies() {
    let s = spec(Some("Replica"));
    assert!(matches!(run(&s, false, &[], IMAGE), Task::Apply { .. }));
}

#[test]
fn exists_retiring_retires_despite_collision() {
    let s = spec(Some("Retiring"));
    let names = vec!["pvc-2".to_string()];
    let object = ObjectView {
        spec: &s,
        deleting: false,
        finalizers: &own_finalizer(),
        collides_with: &names,
    };
    assert_eq!(decide(NODE, VOLUME, Some(&object), IMAGE), Task::RetireReplica);
    assert_eq!(decide(NODE, VOLUME, Some(&object), TRACES), Task::RetireReplica);
    assert_eq!(decide(NODE, VOLUME, Some(&object), NONE), Task::FinishRetire);
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
                                    for (address, duplicate) in
                                        [("192.0.2.5", false), ("bogus", false), ("192.0.2.5", true)]
                                    {
                                        let mut s = spec_of(kind, initial_sync, address);
                                        if duplicate && let Some(own) = s.nodes.iter().find(|n| n.name == NODE).cloned()
                                        {
                                            s.nodes.push(own);
                                        }
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
                                            "{object_state} {kind:?} fin={own_fin} {local:?} coll={collision} {initial_sync} {address} dup={duplicate} -> {task:?}"
                                        );
                                        let listed_twice = duplicate && kind.is_some() && object_state != "none";
                                        // A node listed twice is refused, whatever else holds.
                                        if listed_twice {
                                            assert!(matches!(task, Task::Refuse(Refusal::InvalidSpec(_))), "{ctx}");
                                        }
                                        let may_delete =
                                            object_state == "deleting" && kind == Some("Replica") && own_fin;
                                        // R10.
                                        if task == Task::DeleteReplica {
                                            assert!(may_delete, "{ctx}");
                                        }
                                        if may_delete && (image || traces) && !listed_twice {
                                            assert_eq!(task, Task::DeleteReplica, "{ctx}");
                                        }
                                        // DropFinalizer: this node's finalizer on a deleting
                                        // object with nothing local, for a Replica and for a
                                        // node that is not one (a stray finalizer).
                                        if task == Task::DropFinalizer {
                                            assert!(
                                                object_state == "deleting" && own_fin && !image && !traces,
                                                "{ctx}"
                                            );
                                        }
                                        if object_state == "deleting" && own_fin && !image && !traces && !listed_twice {
                                            assert_eq!(task, Task::DropFinalizer, "{ctx}");
                                        }
                                        // Never clean a node that has a backing file.
                                        if image {
                                            assert_ne!(task, Task::CleanDiskless, "{ctx}");
                                        }
                                        // Apply only for a live object the node is a member of.
                                        if matches!(task, Task::Apply { .. }) {
                                            assert!(object_state == "exists" && kind.is_some(), "{ctx}");
                                            assert!(!collision && address != "bogus" && !listed_twice, "{ctx}");
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
    assert_eq!(checked, 3 * 4 * 2 * 2 * 2 * 2 * 2 * 2 * 3);
}
