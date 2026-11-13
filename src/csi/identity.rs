//! The CSI Identity service: plugin name, capabilities and readiness.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tonic::{Request, Response, Status};

use super::proto::identity_server::Identity;
use super::proto::plugin_capability::{self, service};
use super::proto::{
    GetPluginCapabilitiesRequest, GetPluginCapabilitiesResponse, GetPluginInfoRequest, GetPluginInfoResponse,
    PluginCapability, ProbeRequest, ProbeResponse,
};

/// The CSI driver name; also the API group of the custom resources.
pub const DRIVER_NAME: &str = "ambar.balookrd.github.io";

pub struct IdentityService {
    /// The agent's flag (see `Agent::ready`); `Probe` reports its value.
    pub ready: Arc<AtomicBool>,
}

#[tonic::async_trait]
impl Identity for IdentityService {
    async fn get_plugin_info(
        &self,
        _request: Request<GetPluginInfoRequest>,
    ) -> Result<Response<GetPluginInfoResponse>, Status> {
        Ok(Response::new(GetPluginInfoResponse {
            name: DRIVER_NAME.to_string(),
            vendor_version: env!("CARGO_PKG_VERSION").to_string(),
            manifest: Default::default(),
        }))
    }

    async fn get_plugin_capabilities(
        &self,
        _request: Request<GetPluginCapabilitiesRequest>,
    ) -> Result<Response<GetPluginCapabilitiesResponse>, Status> {
        let service = |kind: service::Type| PluginCapability {
            r#type: Some(plugin_capability::Type::Service(plugin_capability::Service {
                r#type: kind as i32,
            })),
        };
        Ok(Response::new(GetPluginCapabilitiesResponse {
            capabilities: vec![
                service(service::Type::ControllerService),
                service(service::Type::VolumeAccessibilityConstraints),
            ],
        }))
    }

    async fn probe(&self, _request: Request<ProbeRequest>) -> Result<Response<ProbeResponse>, Status> {
        Ok(Response::new(ProbeResponse {
            ready: Some(self.ready.load(Ordering::Acquire)),
        }))
    }
}

#[cfg(test)]
#[path = "tests/identity.rs"]
mod tests;
