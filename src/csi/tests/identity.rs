use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tonic::Request;

use super::*;
use crate::csi::proto::plugin_capability::{Type, service};

fn service_with(ready: bool) -> IdentityService {
    IdentityService {
        ready: Arc::new(AtomicBool::new(ready)),
    }
}

#[tokio::test]
async fn plugin_info_names_the_driver_and_the_crate_version() {
    let info = service_with(true)
        .get_plugin_info(Request::new(GetPluginInfoRequest {}))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(info.name, "ambar.balookrd.github.io");
    assert_eq!(info.vendor_version, env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn capabilities_are_the_controller_service_and_accessibility_constraints() {
    let caps = service_with(true)
        .get_plugin_capabilities(Request::new(GetPluginCapabilitiesRequest {}))
        .await
        .unwrap()
        .into_inner()
        .capabilities;
    let kinds: Vec<i32> = caps
        .into_iter()
        .map(|c| match c.r#type {
            Some(Type::Service(s)) => s.r#type,
            other => panic!("unexpected capability {other:?}"),
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            service::Type::ControllerService as i32,
            service::Type::VolumeAccessibilityConstraints as i32,
        ]
    );
}

#[tokio::test]
async fn probe_reports_the_ready_flag() {
    let svc = service_with(true);
    let reply = svc.probe(Request::new(ProbeRequest {})).await.unwrap().into_inner();
    assert_eq!(reply.ready, Some(true));
}

#[tokio::test]
async fn probe_reports_not_ready_while_the_agent_is_not() {
    let svc = service_with(false);
    let reply = svc.probe(Request::new(ProbeRequest {})).await.unwrap().into_inner();
    assert_eq!(reply.ready, Some(false));

    svc.ready.store(true, std::sync::atomic::Ordering::Release);
    let reply = svc.probe(Request::new(ProbeRequest {})).await.unwrap().into_inner();
    assert_eq!(reply.ready, Some(true));
}
