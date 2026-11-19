use std::time::{Duration, Instant};

use super::*;

#[test]
fn unmounted_volume_has_no_action() {
    let mut tracker = QuorumTracker::new(Duration::from_secs(30));
    let now = Instant::now();
    assert_eq!(tracker.update("v1", false, false, now), QuorumAction::None);
    assert_eq!(tracker.update("v1", false, true, now), QuorumAction::None);
}

#[test]
fn healthy_mounted_volume_has_no_action() {
    let mut tracker = QuorumTracker::new(Duration::from_secs(30));
    let now = Instant::now();
    assert_eq!(tracker.update("v1", true, true, now), QuorumAction::None);
}

#[test]
fn quorum_loss_starts_grace_period() {
    let mut tracker = QuorumTracker::new(Duration::from_secs(30));
    let t0 = Instant::now();

    assert_eq!(
        tracker.update("v1", true, false, t0),
        QuorumAction::WithinGracePeriod(Duration::ZERO)
    );

    let t1 = t0 + Duration::from_secs(10);
    assert_eq!(
        tracker.update("v1", true, false, t1),
        QuorumAction::WithinGracePeriod(Duration::from_secs(10))
    );

    let t2 = t0 + Duration::from_secs(29);
    assert_eq!(
        tracker.update("v1", true, false, t2),
        QuorumAction::WithinGracePeriod(Duration::from_secs(29))
    );
}

#[test]
fn grace_period_expiration_triggers_eviction_once() {
    let mut tracker = QuorumTracker::new(Duration::from_secs(30));
    let t0 = Instant::now();

    tracker.update("v1", true, false, t0);

    let t_expire = t0 + Duration::from_secs(30);
    assert_eq!(tracker.update("v1", true, false, t_expire), QuorumAction::EvictPods);

    // Subsequent calls stay in AlreadyEvicted until quorum recovers
    let t_later = t0 + Duration::from_secs(35);
    assert_eq!(tracker.update("v1", true, false, t_later), QuorumAction::AlreadyEvicted);
}

#[test]
fn recovering_quorum_resets_timer() {
    let mut tracker = QuorumTracker::new(Duration::from_secs(30));
    let t0 = Instant::now();

    tracker.update("v1", true, false, t0);
    let t1 = t0 + Duration::from_secs(15);
    tracker.update("v1", true, false, t1);

    // Quorum recovers
    let t2 = t0 + Duration::from_secs(20);
    assert_eq!(tracker.update("v1", true, true, t2), QuorumAction::None);

    // Lost again: timer restarts from scratch
    let t3 = t0 + Duration::from_secs(25);
    assert_eq!(
        tracker.update("v1", true, false, t3),
        QuorumAction::WithinGracePeriod(Duration::ZERO)
    );

    let t4 = t3 + Duration::from_secs(30);
    assert_eq!(tracker.update("v1", true, false, t4), QuorumAction::EvictPods);
}

#[test]
fn unmounting_volume_clears_timer() {
    let mut tracker = QuorumTracker::new(Duration::from_secs(30));
    let t0 = Instant::now();

    tracker.update("v1", true, false, t0);

    // Pod unmounted
    let t1 = t0 + Duration::from_secs(10);
    assert_eq!(tracker.update("v1", false, false, t1), QuorumAction::None);

    // Mounted again without quorum: restarts
    let t2 = t0 + Duration::from_secs(15);
    assert_eq!(
        tracker.update("v1", true, false, t2),
        QuorumAction::WithinGracePeriod(Duration::ZERO)
    );
}

#[test]
fn forget_removes_tracked_volume() {
    let mut tracker = QuorumTracker::new(Duration::from_secs(30));
    let t0 = Instant::now();
    tracker.update("v1", true, false, t0);
    tracker.forget("v1");

    let t1 = t0 + Duration::from_secs(35);
    assert_eq!(
        tracker.update("v1", true, false, t1),
        QuorumAction::WithinGracePeriod(Duration::ZERO)
    );
}

#[tokio::test]
async fn check_volume_quorum_triggers_pod_eviction_after_grace_period() {
    let fake = crate::tests::volumes::FakeVolumes::default();
    let pod = k8s_openapi::api::core::v1::Pod {
        metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
            name: Some("test-pod".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: Some(k8s_openapi::api::core::v1::PodSpec {
            node_name: Some("dv-1".to_string()),
            volumes: Some(vec![k8s_openapi::api::core::v1::Volume {
                name: "v1".to_string(),
                ..Default::default()
            }]),
            ..Default::default()
        }),
        ..Default::default()
    };
    fake.pods.lock().unwrap().push(pod);

    let mut tracker = QuorumTracker::new(Duration::from_secs(30));
    let t0 = Instant::now();

    let check = |is_mounted, has_quorum, now| VolumeQuorumCheck {
        node: "dv-1",
        volume: "v1",
        is_mounted,
        has_quorum,
        mounted_uids: &[],
        now,
    };

    // t0: quorum lost, timer starts
    let res0 = check_volume_quorum(&mut tracker, &fake, check(true, false, t0))
        .await
        .unwrap();
    assert_eq!(res0, None);

    // 10s: no eviction
    let res10 = check_volume_quorum(&mut tracker, &fake, check(true, false, t0 + Duration::from_secs(10)))
        .await
        .unwrap();
    assert_eq!(res10, None);
    assert!(fake.calls.lock().unwrap().is_empty());

    // 30s: grace period expired -> eviction
    let res30 = check_volume_quorum(&mut tracker, &fake, check(true, false, t0 + Duration::from_secs(30)))
        .await
        .unwrap();
    assert_eq!(res30, Some(vec!["test-pod".to_string()]));
    assert_eq!(fake.calls.lock().unwrap().as_slice(), &["delete-pod dv-1 v1 test-pod"]);

    // 35s: already evicted
    let res35 = check_volume_quorum(&mut tracker, &fake, check(true, false, t0 + Duration::from_secs(35)))
        .await
        .unwrap();
    assert_eq!(res35, None);
    assert_eq!(fake.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn check_volume_quorum_returns_api_error_on_failure() {
    let fake = crate::tests::volumes::FakeVolumes::default();
    fake.fail.store(true, std::sync::atomic::Ordering::SeqCst);
    let mut tracker = QuorumTracker::new(Duration::ZERO);
    let check = VolumeQuorumCheck {
        node: "dv-1",
        volume: "v1",
        is_mounted: true,
        has_quorum: false,
        mounted_uids: &[],
        now: Instant::now(),
    };
    let res = check_volume_quorum(&mut tracker, &fake, check).await;
    assert!(res.is_err());
}

#[tokio::test]
async fn check_volume_quorum_retries_eviction_after_api_failure() {
    let fake = crate::tests::volumes::FakeVolumes::default();
    let pod = k8s_openapi::api::core::v1::Pod {
        metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
            name: Some("test-pod".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: Some(k8s_openapi::api::core::v1::PodSpec {
            node_name: Some("dv-1".to_string()),
            volumes: Some(vec![k8s_openapi::api::core::v1::Volume {
                name: "v1".to_string(),
                ..Default::default()
            }]),
            ..Default::default()
        }),
        ..Default::default()
    };
    fake.pods.lock().unwrap().push(pod);

    fake.fail.store(true, std::sync::atomic::Ordering::SeqCst);
    let mut tracker = QuorumTracker::new(Duration::ZERO);
    let check = VolumeQuorumCheck {
        node: "dv-1",
        volume: "v1",
        is_mounted: true,
        has_quorum: false,
        mounted_uids: &[],
        now: Instant::now(),
    };
    // First attempt fails due to fake API failure
    let res = check_volume_quorum(&mut tracker, &fake, check).await;
    assert!(res.is_err());

    // API recovers: next check must retry eviction and succeed
    fake.fail.store(false, std::sync::atomic::Ordering::SeqCst);
    let check2 = VolumeQuorumCheck {
        node: "dv-1",
        volume: "v1",
        is_mounted: true,
        has_quorum: false,
        mounted_uids: &[],
        now: Instant::now(),
    };
    let res2 = check_volume_quorum(&mut tracker, &fake, check2).await.unwrap();
    assert_eq!(res2, Some(vec!["test-pod".to_string()]));
}
