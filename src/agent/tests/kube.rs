use std::fmt::Debug;
use std::future::Future;

use serde_json::json;
use tokio::time::Instant;

use super::*;
use crate::tests::volumes::{hanging_client, typed};

const EXPIRED: &str = "the API server did not answer within 30 s";

/// Runs `call` against an API server that never answers, on paused time. The
/// outer guard keeps a call without a limit of its own from hanging the test.
async fn expires<T: Debug>(call: impl Future<Output = Result<T, ApiError>>) {
    let start = Instant::now();
    let result = tokio::time::timeout(API_CALL_TIMEOUT * 2, call)
        .await
        .expect("the call has no time limit of its own");
    assert_eq!(start.elapsed(), API_CALL_TIMEOUT);
    match result {
        Err(ApiError::Other(message)) => assert_eq!(message, EXPIRED),
        other => panic!("expected the timeout error, got {other:?}"),
    }
}

fn volumes() -> KubeVolumes {
    KubeVolumes::new(hanging_client())
}

fn object() -> ReplicatedVolume {
    typed(&json!({
        "apiVersion": "ambar.balookrd.github.io/v1alpha1",
        "kind": "ReplicatedVolume",
        "metadata": {"name": "v"},
        "spec": {"sizeBytes": 1073741824u64, "protocol": "C", "minor": 1, "port": 7000, "maxPeers": 1, "nodes": []},
    }))
}

#[test]
fn the_limit_is_thirty_seconds() {
    assert_eq!(API_CALL_TIMEOUT, std::time::Duration::from_secs(30));
}

#[tokio::test(start_paused = true)]
async fn get_has_a_time_limit() {
    expires(volumes().get("v")).await;
}

#[tokio::test(start_paused = true)]
async fn list_before_the_first_watch_listing_has_a_time_limit() {
    expires(volumes().list()).await;
}

#[tokio::test(start_paused = true)]
async fn list_fresh_has_a_time_limit() {
    expires(volumes().list_fresh()).await;
}

#[tokio::test(start_paused = true)]
async fn create_has_a_time_limit() {
    expires(volumes().create(&object())).await;
}

#[tokio::test(start_paused = true)]
async fn delete_has_a_time_limit() {
    expires(volumes().delete("v")).await;
}

#[tokio::test(start_paused = true)]
async fn delete_exact_has_a_time_limit() {
    expires(volumes().delete_exact("v", "uid")).await;
}

#[tokio::test(start_paused = true)]
async fn a_json_patch_has_a_time_limit() {
    let ops = vec![json!({"op": "test", "path": "/metadata/uid", "value": "uid"})];
    expires(Objects::patch(&volumes(), "v", ops)).await;
}

#[tokio::test(start_paused = true)]
async fn a_json_patch_of_the_status_has_a_time_limit() {
    let ops = vec![json!({"op": "test", "path": "/metadata/uid", "value": "uid"})];
    expires(Objects::patch_status(&volumes(), "v", ops)).await;
}

#[tokio::test(start_paused = true)]
async fn the_server_side_apply_of_the_status_has_a_time_limit() {
    expires(volumes().apply_node_status("v", "n1", None)).await;
}

#[tokio::test(start_paused = true)]
async fn an_edit_that_reads_first_ends_with_the_timeout_error() {
    // The read of `patch_retrying` is the call that expires; the edit is not
    // reported as "no such object" or "nothing to change".
    expires(volumes().remove_finalizer("v", "uid", "f")).await;
}

#[tokio::test(start_paused = true)]
async fn edit_members_has_a_time_limit() {
    expires(volumes().edit_members(
        "v",
        "uid",
        &[],
        &crate::replicas::Edit {
            nodes: vec![],
            add_finalizer: None,
        },
    ))
    .await;
}

#[test]
fn edit_members_ops_returns_none_when_expected_differs() {
    use crate::crd::{MemberNode, NodeKind};
    let obj = object();
    let diff = vec![MemberNode {
        name: "n1".into(),
        node_id: 1,
        address: "192.0.2.1".into(),
        kind: NodeKind::Replica,
    }];
    let edit = crate::replicas::Edit {
        nodes: vec![],
        add_finalizer: None,
    };
    assert_eq!(edit_members_ops(&obj, &diff, &edit), None);
}

#[test]
fn edit_members_ops_without_finalizer_has_two_ops() {
    let obj = object();
    let edit = crate::replicas::Edit {
        nodes: vec![],
        add_finalizer: None,
    };
    let ops = edit_members_ops(&obj, &[], &edit).unwrap();
    assert_eq!(ops.len(), 2);
    assert_eq!(ops[0], json!({"op": "test", "path": "/spec/nodes", "value": []}));
    assert_eq!(ops[1], json!({"op": "replace", "path": "/spec/nodes", "value": []}));
}

#[test]
fn edit_members_ops_with_new_finalizer_and_existing_list_has_four_ops() {
    let mut obj = object();
    obj.metadata.finalizers = Some(vec!["f1".into()]);
    let edit = crate::replicas::Edit {
        nodes: vec![],
        add_finalizer: Some("f2".into()),
    };
    let ops = edit_members_ops(&obj, &[], &edit).unwrap();
    assert_eq!(ops.len(), 4);
    assert_eq!(ops[0], json!({"op": "test", "path": "/spec/nodes", "value": []}));
    assert_eq!(ops[1], json!({"op": "replace", "path": "/spec/nodes", "value": []}));
    assert_eq!(
        ops[2],
        json!({"op": "test", "path": "/metadata/finalizers", "value": ["f1"]})
    );
    assert_eq!(
        ops[3],
        json!({"op": "add", "path": "/metadata/finalizers/-", "value": "f2"})
    );
}

#[test]
fn edit_members_ops_with_missing_finalizers_field_has_three_ops() {
    let mut obj = object();
    obj.metadata.finalizers = None;
    let edit = crate::replicas::Edit {
        nodes: vec![],
        add_finalizer: Some("f1".into()),
    };
    let ops = edit_members_ops(&obj, &[], &edit).unwrap();
    assert_eq!(ops.len(), 3);
    assert_eq!(ops[0], json!({"op": "test", "path": "/spec/nodes", "value": []}));
    assert_eq!(ops[1], json!({"op": "replace", "path": "/spec/nodes", "value": []}));
    assert_eq!(
        ops[2],
        json!({"op": "add", "path": "/metadata/finalizers", "value": ["f1"]})
    );
}

#[test]
fn edit_members_ops_when_finalizer_already_present_has_two_ops() {
    let mut obj = object();
    obj.metadata.finalizers = Some(vec!["f1".into()]);
    let edit = crate::replicas::Edit {
        nodes: vec![],
        add_finalizer: Some("f1".into()),
    };
    let ops = edit_members_ops(&obj, &[], &edit).unwrap();
    assert_eq!(ops.len(), 2);
    assert_eq!(ops[0], json!({"op": "test", "path": "/spec/nodes", "value": []}));
    assert_eq!(ops[1], json!({"op": "replace", "path": "/spec/nodes", "value": []}));
}
