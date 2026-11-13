//! The CSI Controller service. `CreateVolume` places a volume (`placement::place`
//! over the node map and every existing object) and creates the
//! `ReplicatedVolume` object, then waits until every Replica node reports its
//! disk `UpToDate`. `DeleteVolume` deletes the object and waits until it is
//! gone; the node agents remove their copies under their own safety rules, so
//! a volume that is still mounted keeps the object (and `DeleteVolume` answers
//! `DEADLINE_EXCEEDED` again and again until the copies can go).
//!
//! Concurrent `CreateVolume` calls. The part from the read of the object to
//! the end of its creation runs under `placing`, an async mutex of this
//! process: two calls for different names cannot both pick the same minor and
//! port from the same listing, and two calls for the same name see the
//! object of the first. Before it lets go of the mutex, a call waits until
//! `Volumes::list` returns the object it has created (`await_listed`): the
//! agent's `list` reads the reflector cache, which can lag behind a create by
//! the watch latency. The wait for the volume to become ready runs outside the
//! mutex. Another Controller instance (a leader handover with a request still
//! in flight) is not covered by the mutex; the agents refuse to configure two
//! objects with one minor or port (`membership::collisions`).
//!
//! Every step is idempotent: the object name is the volume name, a repeated
//! call finds the object and goes on to the wait.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::{Instant, sleep};
use tonic::{Request, Response, Status};

use super::proto::controller_server::Controller;
use super::proto::controller_service_capability::{self, rpc};
use super::proto::validate_volume_capabilities_response::Confirmed;
use super::proto::volume_capability::{AccessType, access_mode::Mode};
use super::proto::{
    ControllerExpandVolumeRequest, ControllerExpandVolumeResponse, ControllerGetCapabilitiesRequest,
    ControllerGetCapabilitiesResponse, ControllerGetVolumeHealthRequest, ControllerGetVolumeHealthResponse,
    ControllerGetVolumeRequest, ControllerGetVolumeResponse, ControllerListVolumeHealthRequest,
    ControllerListVolumeHealthResponse, ControllerModifyVolumeRequest, ControllerModifyVolumeResponse,
    ControllerPublishVolumeRequest, ControllerPublishVolumeResponse, ControllerServiceCapability,
    ControllerUnpublishVolumeRequest, ControllerUnpublishVolumeResponse, CreateSnapshotRequest, CreateSnapshotResponse,
    CreateVolumeRequest, CreateVolumeResponse, DeleteSnapshotRequest, DeleteSnapshotResponse, DeleteVolumeRequest,
    DeleteVolumeResponse, GetCapacityRequest, GetCapacityResponse, GetSnapshotRequest, GetSnapshotResponse,
    ListSnapshotsRequest, ListSnapshotsResponse, ListVolumesRequest, ListVolumesResponse,
    ValidateVolumeCapabilitiesRequest, ValidateVolumeCapabilitiesResponse, Volume, VolumeCapability,
};
use crate::agent::kube::{ApiError, Created, Volumes};
use crate::agent::nodes::NodeMap;
use crate::agent::status::all_replicas_up_to_date;
use crate::crd::{NodeKind, ReplicatedVolume, ReplicatedVolumeStatus};
use crate::membership::initial_finalizers;
use crate::model::valid_volume_name;
use crate::placement::{Existing, PlaceError, Request as PlaceRequest, parse_parameters, place, round_up_size};

/// Key of the topology segment that carries the Kubernetes node name.
pub const TOPOLOGY_KEY: &str = "ambar.balookrd.github.io/node";

/// Times a `CreateVolume` goes back to the read of the object after another
/// writer has created it in between.
const CREATE_ATTEMPTS: usize = 3;
/// `await_listed`: how often the list is read and for how long at most.
const LISTED_POLL: Duration = Duration::from_millis(50);
const LISTED_WAIT: Duration = Duration::from_secs(5);

pub struct ControllerService {
    pub volumes: Arc<dyn Volumes>,
    pub nodes: Arc<dyn NodeMap>,
    /// How long `CreateVolume` waits for the replicas to become `UpToDate`.
    pub create_wait: Duration,
    /// How long `DeleteVolume` waits for the object to disappear.
    pub delete_wait: Duration,
    /// Interval between the reads of the object during both waits.
    pub poll: Duration,
    /// Serialises the placement and the creation (see the module comment).
    placing: Mutex<()>,
}

impl ControllerService {
    pub fn new(volumes: Arc<dyn Volumes>, nodes: Arc<dyn NodeMap>) -> Self {
        Self {
            volumes,
            nodes,
            create_wait: Duration::from_secs(60),
            delete_wait: Duration::from_secs(60),
            poll: Duration::from_millis(500),
            placing: Mutex::new(()),
        }
    }
}

/// What a call to the API server that failed means to the caller: a refused
/// object is a bug of this build, anything else is worth a retry.
fn api_status(context: &str, error: ApiError) -> Status {
    match error {
        ApiError::Rejected(why) => Status::internal(format!("{context}: the API server rejected the request: {why}")),
        other => Status::unavailable(format!("{context}: {other}")),
    }
}

/// `Err` carries the reason why the capability is not supported; it names the value.
fn check_capability(capability: &VolumeCapability) -> Result<(), String> {
    match &capability.access_type {
        Some(AccessType::Mount(mount)) => {
            if !mount.fs_type.is_empty() && mount.fs_type != "ext4" {
                return Err(format!("fs_type {:?} is not supported, only ext4", mount.fs_type));
            }
        }
        Some(AccessType::Block(_)) => return Err("block volumes are not supported, only mount".to_string()),
        None => return Err("the volume capability has no access type, only mount is supported".to_string()),
    }
    let Some(access) = capability.access_mode else {
        return Err("the volume capability has no access mode".to_string());
    };
    match Mode::try_from(access.mode) {
        Ok(
            Mode::SingleNodeWriter
            | Mode::SingleNodeSingleWriter
            | Mode::SingleNodeMultiWriter
            | Mode::SingleNodeReaderOnly,
        ) => Ok(()),
        Ok(other) => Err(format!(
            "access mode {} is not supported, only the SINGLE_NODE_* modes are",
            other.as_str_name()
        )),
        Err(_) => Err(format!("access mode {} is not known", access.mode)),
    }
}

fn check_capabilities(capabilities: &[VolumeCapability]) -> Result<(), String> {
    if capabilities.is_empty() {
        return Err("volume_capabilities is empty".to_string());
    }
    capabilities.iter().try_for_each(check_capability)
}

/// The node the scheduler picked: the `TOPOLOGY_KEY` segment of the first
/// preferred topology, or of the first requisite one when there is no preferred.
pub fn selected_node(request: &CreateVolumeRequest) -> Option<String> {
    let requirements = request.accessibility_requirements.as_ref()?;
    let first = match requirements.preferred.first() {
        Some(preferred) => preferred,
        None => requirements.requisite.first()?,
    };
    first.segments.get(TOPOLOGY_KEY).cloned()
}

/// `INVALID_ARGUMENT` for what the request or the cluster makes impossible,
/// `RESOURCE_EXHAUSTED` for what is used up.
fn place_status(error: &PlaceError) -> Status {
    match error {
        PlaceError::Replicas(_)
        | PlaceError::NotEnoughNodes { .. }
        | PlaceError::UnknownSelectedNode(_)
        | PlaceError::MaxPeers { .. } => Status::invalid_argument(error.to_string()),
        PlaceError::NoCapacity { .. } | PlaceError::NoFreeIndex => Status::resource_exhausted(error.to_string()),
    }
}

fn existing_of(object: &ReplicatedVolume) -> Existing {
    Existing {
        minor: object.spec.minor,
        port: object.spec.port,
        replica_nodes: object
            .spec
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Replica)
            .map(|n| n.name.clone())
            .collect(),
        size_bytes: object.spec.size_bytes,
    }
}

fn replicas_of(object: &ReplicatedVolume) -> usize {
    object.spec.nodes.iter().filter(|n| n.kind == NodeKind::Replica).count()
}

fn volume_response(object: &ReplicatedVolume, name: &str) -> CreateVolumeResponse {
    CreateVolumeResponse {
        volume: Some(Volume {
            capacity_bytes: i64::try_from(object.spec.size_bytes).unwrap_or(i64::MAX),
            volume_id: name.to_string(),
            volume_context: Default::default(),
            content_source: None,
            accessible_topology: Vec::new(),
        }),
    }
}

impl ControllerService {
    /// Steps 5 to 7: the object `name` as it stands or as it is created now.
    async fn find_or_create(&self, name: &str, wanted: &PlaceRequest) -> Result<(), Status> {
        for _ in 0..CREATE_ATTEMPTS {
            let _placing = self.placing.lock().await;
            if let Some(object) = self
                .volumes
                .get(name)
                .await
                .map_err(|e| api_status("cannot read the volume", e))?
            {
                return same_volume(name, &object, wanted);
            }
            let nodes = self
                .nodes
                .load()
                .await
                .map_err(|e| api_status("cannot read the node map", e))?;
            // Objects that are being deleted count: their minor, port and
            // budget are in use until they are gone.
            let existing: Vec<Existing> = self
                .volumes
                .list()
                .await
                .map_err(|e| api_status("cannot list the volumes", e))?
                .iter()
                .map(existing_of)
                .collect();
            let spec = place(wanted, &nodes, &existing).map_err(|e| place_status(&e))?;
            let mut object = ReplicatedVolume::new(name, spec);
            object.metadata.finalizers = Some(initial_finalizers(&object.spec));
            match self
                .volumes
                .create(&object)
                .await
                .map_err(|e| api_status("cannot create the volume", e))?
            {
                Created::New => {
                    self.await_listed(name).await;
                    return Ok(());
                }
                // Somebody else has created it since the read: compare again.
                Created::Exists => {}
            }
        }
        Err(Status::aborted(format!(
            "volume {name:?} was created by another writer {CREATE_ATTEMPTS} times in a row; retry"
        )))
    }

    /// Waits until the list shows the object `name` (or `LISTED_WAIT` has
    /// passed): the next placement must see its minor and port. A failing list
    /// ends the wait too; the next call reads the list again anyway.
    async fn await_listed(&self, name: &str) {
        let deadline = Instant::now() + LISTED_WAIT;
        loop {
            match self.volumes.list().await {
                Ok(list) if list.iter().any(|o| o.metadata.name.as_deref() == Some(name)) => return,
                Ok(_) if Instant::now() < deadline => sleep(LISTED_POLL).await,
                _ => return,
            }
        }
    }

    /// Step 8: reads the object every `poll` until all Replica nodes report
    /// `UpToDate`; returns the object as last read.
    async fn wait_until_ready(&self, name: &str) -> Result<ReplicatedVolume, Status> {
        let deadline = Instant::now() + self.create_wait;
        loop {
            let object = self
                .volumes
                .get(name)
                .await
                .map_err(|e| api_status("cannot read the volume", e))?
                .ok_or_else(|| Status::aborted(format!("volume {name:?} disappeared while it was being created")))?;
            if object.metadata.deletion_timestamp.is_some() {
                return Err(Status::aborted(format!("volume {name:?} is being deleted")));
            }
            let status = object.status.clone().unwrap_or_default();
            if all_replicas_up_to_date(&object.spec, &status) {
                return Ok(object);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(Status::deadline_exceeded(not_ready_message(
                    name,
                    &object.spec,
                    &status,
                )));
            }
            sleep(self.poll.min(deadline - now)).await;
        }
    }
}

fn not_ready_message(name: &str, spec: &crate::crd::ReplicatedVolumeSpec, status: &ReplicatedVolumeStatus) -> String {
    let waiting: Vec<&str> = spec
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Replica)
        .filter(|n| !status.nodes.iter().any(|s| s.name == n.name && s.disk == "UpToDate"))
        .map(|n| n.name.as_str())
        .collect();
    format!(
        "volume {name:?} is not ready: no UpToDate disk reported yet by {}",
        waiting.join(", ")
    )
}

/// Step 5 and 6: the object that is there has the requested parameters or not.
fn same_volume(name: &str, object: &ReplicatedVolume, wanted: &PlaceRequest) -> Result<(), Status> {
    if object.metadata.deletion_timestamp.is_some() {
        return Err(Status::aborted(format!(
            "volume {name:?} is being deleted; retry when it is gone"
        )));
    }
    let spec = &object.spec;
    let mut differing = Vec::new();
    if spec.size_bytes != wanted.size_bytes {
        differing.push(format!("size {} (requested {})", spec.size_bytes, wanted.size_bytes));
    }
    if spec.protocol != wanted.protocol {
        differing.push(format!(
            "protocol {:?} (requested {:?})",
            spec.protocol, wanted.protocol
        ));
    }
    if spec.max_peers != wanted.max_peers {
        differing.push(format!("maxPeers {} (requested {})", spec.max_peers, wanted.max_peers));
    }
    if replicas_of(object) != wanted.replicas {
        differing.push(format!(
            "replicas {} (requested {})",
            replicas_of(object),
            wanted.replicas
        ));
    }
    if differing.is_empty() {
        Ok(())
    } else {
        Err(Status::already_exists(format!(
            "volume {name:?} exists with another {}",
            differing.join(", ")
        )))
    }
}

#[tonic::async_trait]
impl Controller for ControllerService {
    async fn create_volume(
        &self,
        request: Request<CreateVolumeRequest>,
    ) -> Result<Response<CreateVolumeResponse>, Status> {
        let request = request.into_inner();
        let name = request.name.as_str();
        // Step 1.
        if !valid_volume_name(name) {
            return Err(Status::invalid_argument(format!(
                "name {name:?} is not a valid volume name: use 1-63 characters a-z, 0-9 and '-', not starting with '-' and not \"all\""
            )));
        }
        // Steps 2 and 3.
        check_capabilities(&request.volume_capabilities)
            .map_err(|why| Status::invalid_argument(format!("volume_capabilities: {why}")))?;
        if request.volume_content_source.is_some() {
            return Err(Status::invalid_argument(
                "volume_content_source is not supported: volumes cannot be created from a snapshot or another volume",
            ));
        }
        // Step 4.
        let parameters: BTreeMap<String, String> = request.parameters.clone().into_iter().collect();
        let (replicas, protocol, max_peers) =
            parse_parameters(&parameters).map_err(|e| Status::invalid_argument(format!("parameters: {e}")))?;
        let (required, limit) = request
            .capacity_range
            .map_or((0, 0), |range| (range.required_bytes, range.limit_bytes));
        let size_bytes = round_up_size(required).map_err(|e| match e {
            crate::placement::ParamError::SizeTooLarge(_) => Status::out_of_range(format!("capacity_range: {e}")),
            other => Status::invalid_argument(format!("capacity_range: {other}")),
        })?;
        if limit < 0 {
            return Err(Status::invalid_argument(format!(
                "capacity_range.limit_bytes {limit} is negative"
            )));
        }
        if limit != 0 && u64::try_from(limit).is_ok_and(|limit| limit < size_bytes) {
            return Err(Status::out_of_range(format!(
                "capacity_range.limit_bytes {limit} is below the size {size_bytes} (the requested size rounded up to 4096)"
            )));
        }
        let wanted = PlaceRequest {
            size_bytes,
            replicas,
            protocol,
            max_peers,
            selected_node: selected_node(&request),
        };

        // Steps 5 to 7, then 8 and 9.
        self.find_or_create(name, &wanted).await?;
        let object = self.wait_until_ready(name).await?;
        Ok(Response::new(volume_response(&object, name)))
    }

    async fn delete_volume(
        &self,
        request: Request<DeleteVolumeRequest>,
    ) -> Result<Response<DeleteVolumeResponse>, Status> {
        let id = request.into_inner().volume_id;
        if id.is_empty() {
            return Err(Status::invalid_argument("volume_id is empty"));
        }
        // An id that is no volume name was never handed out by `CreateVolume`.
        if !valid_volume_name(&id) {
            return Ok(Response::new(DeleteVolumeResponse {}));
        }
        let read = |context: &'static str| async { self.volumes.get(&id).await.map_err(|e| api_status(context, e)) };
        if read("cannot read the volume").await?.is_none() {
            return Ok(Response::new(DeleteVolumeResponse {}));
        }
        self.volumes
            .delete(&id)
            .await
            .map_err(|e| api_status("cannot delete the volume", e))?;
        let deadline = Instant::now() + self.delete_wait;
        loop {
            if read("cannot read the volume").await?.is_none() {
                return Ok(Response::new(DeleteVolumeResponse {}));
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(Status::deadline_exceeded(format!(
                    "volume {id:?} is deleted but the node agents have not released it yet (a copy may still be in use)"
                )));
            }
            sleep(self.poll.min(deadline - now)).await;
        }
    }

    async fn validate_volume_capabilities(
        &self,
        request: Request<ValidateVolumeCapabilitiesRequest>,
    ) -> Result<Response<ValidateVolumeCapabilitiesResponse>, Status> {
        let request = request.into_inner();
        let id = request.volume_id.as_str();
        if id.is_empty() {
            return Err(Status::invalid_argument("volume_id is empty"));
        }
        if request.volume_capabilities.is_empty() {
            return Err(Status::invalid_argument("volume_capabilities is empty"));
        }
        let found = valid_volume_name(id)
            && self
                .volumes
                .get(id)
                .await
                .map_err(|e| api_status("cannot read the volume", e))?
                .is_some();
        if !found {
            return Err(Status::not_found(format!("volume {id:?} does not exist")));
        }
        Ok(Response::new(match check_capabilities(&request.volume_capabilities) {
            Ok(()) => ValidateVolumeCapabilitiesResponse {
                confirmed: Some(Confirmed {
                    volume_context: request.volume_context,
                    volume_capabilities: request.volume_capabilities,
                    ..Default::default()
                }),
                message: String::new(),
            },
            Err(why) => ValidateVolumeCapabilitiesResponse {
                confirmed: None,
                message: why,
            },
        }))
    }

    async fn controller_get_capabilities(
        &self,
        _request: Request<ControllerGetCapabilitiesRequest>,
    ) -> Result<Response<ControllerGetCapabilitiesResponse>, Status> {
        Ok(Response::new(ControllerGetCapabilitiesResponse {
            capabilities: vec![ControllerServiceCapability {
                r#type: Some(controller_service_capability::Type::Rpc(
                    controller_service_capability::Rpc {
                        r#type: rpc::Type::CreateDeleteVolume as i32,
                    },
                )),
            }],
        }))
    }

    async fn controller_publish_volume(
        &self,
        _request: Request<ControllerPublishVolumeRequest>,
    ) -> Result<Response<ControllerPublishVolumeResponse>, Status> {
        Err(Status::unimplemented(
            "controller_publish_volume is not implemented yet",
        ))
    }

    async fn controller_unpublish_volume(
        &self,
        _request: Request<ControllerUnpublishVolumeRequest>,
    ) -> Result<Response<ControllerUnpublishVolumeResponse>, Status> {
        Err(Status::unimplemented(
            "controller_unpublish_volume is not implemented yet",
        ))
    }

    async fn list_volumes(
        &self,
        _request: Request<ListVolumesRequest>,
    ) -> Result<Response<ListVolumesResponse>, Status> {
        Err(Status::unimplemented("list_volumes is not implemented yet"))
    }

    async fn controller_list_volume_health(
        &self,
        _request: Request<ControllerListVolumeHealthRequest>,
    ) -> Result<Response<ControllerListVolumeHealthResponse>, Status> {
        Err(Status::unimplemented(
            "controller_list_volume_health is not implemented yet",
        ))
    }

    async fn controller_get_volume_health(
        &self,
        _request: Request<ControllerGetVolumeHealthRequest>,
    ) -> Result<Response<ControllerGetVolumeHealthResponse>, Status> {
        Err(Status::unimplemented(
            "controller_get_volume_health is not implemented yet",
        ))
    }

    async fn get_capacity(
        &self,
        _request: Request<GetCapacityRequest>,
    ) -> Result<Response<GetCapacityResponse>, Status> {
        Err(Status::unimplemented("get_capacity is not implemented yet"))
    }

    async fn create_snapshot(
        &self,
        _request: Request<CreateSnapshotRequest>,
    ) -> Result<Response<CreateSnapshotResponse>, Status> {
        Err(Status::unimplemented("create_snapshot is not implemented yet"))
    }

    async fn delete_snapshot(
        &self,
        _request: Request<DeleteSnapshotRequest>,
    ) -> Result<Response<DeleteSnapshotResponse>, Status> {
        Err(Status::unimplemented("delete_snapshot is not implemented yet"))
    }

    async fn list_snapshots(
        &self,
        _request: Request<ListSnapshotsRequest>,
    ) -> Result<Response<ListSnapshotsResponse>, Status> {
        Err(Status::unimplemented("list_snapshots is not implemented yet"))
    }

    async fn get_snapshot(
        &self,
        _request: Request<GetSnapshotRequest>,
    ) -> Result<Response<GetSnapshotResponse>, Status> {
        Err(Status::unimplemented("get_snapshot is not implemented yet"))
    }

    async fn controller_expand_volume(
        &self,
        _request: Request<ControllerExpandVolumeRequest>,
    ) -> Result<Response<ControllerExpandVolumeResponse>, Status> {
        Err(Status::unimplemented("controller_expand_volume is not implemented yet"))
    }

    async fn controller_get_volume(
        &self,
        _request: Request<ControllerGetVolumeRequest>,
    ) -> Result<Response<ControllerGetVolumeResponse>, Status> {
        Err(Status::unimplemented("controller_get_volume is not implemented yet"))
    }

    async fn controller_modify_volume(
        &self,
        _request: Request<ControllerModifyVolumeRequest>,
    ) -> Result<Response<ControllerModifyVolumeResponse>, Status> {
        Err(Status::unimplemented("controller_modify_volume is not implemented yet"))
    }
}

#[cfg(test)]
#[path = "tests/controller.rs"]
mod tests;
