//! The node map of the cluster: the ConfigMap `ambar-nodes` in the agent's
//! namespace, key `nodes`, a JSON array (see `config::ClusterNodes`).
//!
//! `ConfigMapNodes` reads the ConfigMap with `Api::get_opt` on every call and
//! keeps nothing: an operator's edit of the map is seen by the next
//! `CreateVolume` without a restart.

use async_trait::async_trait;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::{Api, Client};

use super::kube::ApiError;
use crate::config::{ClusterNodes, NODES_CONFIGMAP, NODES_KEY};

#[async_trait]
pub trait NodeMap: Send + Sync {
    async fn load(&self) -> Result<ClusterNodes, ApiError>;
}

pub struct ConfigMapNodes {
    api: Api<ConfigMap>,
}

impl ConfigMapNodes {
    pub fn new(client: Client, namespace: &str) -> Self {
        Self {
            api: Api::namespaced(client, namespace),
        }
    }
}

#[async_trait]
impl NodeMap for ConfigMapNodes {
    async fn load(&self) -> Result<ClusterNodes, ApiError> {
        let map = self
            .api
            .get_opt(NODES_CONFIGMAP)
            .await
            .map_err(|e| ApiError::Other(format!("cannot read the ConfigMap {NODES_CONFIGMAP}: {e}")))?;
        nodes_from_configmap(map.as_ref())
    }
}

/// The node map out of the ConfigMap as read (`None`: there is no such
/// ConfigMap). Every error names the ConfigMap.
pub fn nodes_from_configmap(map: Option<&ConfigMap>) -> Result<ClusterNodes, ApiError> {
    let map = map.ok_or_else(|| ApiError::Other(format!("the ConfigMap {NODES_CONFIGMAP} does not exist")))?;
    let json = map
        .data
        .as_ref()
        .and_then(|data| data.get(NODES_KEY))
        .ok_or_else(|| ApiError::Other(format!("the ConfigMap {NODES_CONFIGMAP} has no key {NODES_KEY:?}")))?;
    ClusterNodes::from_json(json)
        .map_err(|e| ApiError::Other(format!("the key {NODES_KEY:?} of the ConfigMap {NODES_CONFIGMAP}: {e}")))
}

#[cfg(test)]
#[path = "tests/nodes.rs"]
mod tests;
