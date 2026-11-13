//! The CSI Controller service: a placeholder, filled by Task 11.

use tonic::{Request, Response, Status};

use super::proto::controller_server::Controller;
use super::proto::{
    ControllerExpandVolumeRequest, ControllerExpandVolumeResponse, ControllerGetCapabilitiesRequest,
    ControllerGetCapabilitiesResponse, ControllerGetVolumeHealthRequest, ControllerGetVolumeHealthResponse,
    ControllerGetVolumeRequest, ControllerGetVolumeResponse, ControllerListVolumeHealthRequest,
    ControllerListVolumeHealthResponse, ControllerModifyVolumeRequest, ControllerModifyVolumeResponse,
    ControllerPublishVolumeRequest, ControllerPublishVolumeResponse, ControllerUnpublishVolumeRequest,
    ControllerUnpublishVolumeResponse, CreateSnapshotRequest, CreateSnapshotResponse, CreateVolumeRequest,
    CreateVolumeResponse, DeleteSnapshotRequest, DeleteSnapshotResponse, DeleteVolumeRequest, DeleteVolumeResponse,
    GetCapacityRequest, GetCapacityResponse, GetSnapshotRequest, GetSnapshotResponse, ListSnapshotsRequest,
    ListSnapshotsResponse, ListVolumesRequest, ListVolumesResponse, ValidateVolumeCapabilitiesRequest,
    ValidateVolumeCapabilitiesResponse,
};

/// Every call returns `UNIMPLEMENTED` until Task 11.
#[derive(Debug)]
pub struct ControllerService;

#[tonic::async_trait]
impl Controller for ControllerService {
    async fn create_volume(
        &self,
        _request: Request<CreateVolumeRequest>,
    ) -> Result<Response<CreateVolumeResponse>, Status> {
        Err(Status::unimplemented("create_volume is not implemented yet"))
    }

    async fn delete_volume(
        &self,
        _request: Request<DeleteVolumeRequest>,
    ) -> Result<Response<DeleteVolumeResponse>, Status> {
        Err(Status::unimplemented("delete_volume is not implemented yet"))
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

    async fn validate_volume_capabilities(
        &self,
        _request: Request<ValidateVolumeCapabilitiesRequest>,
    ) -> Result<Response<ValidateVolumeCapabilitiesResponse>, Status> {
        Err(Status::unimplemented(
            "validate_volume_capabilities is not implemented yet",
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

    async fn controller_get_capabilities(
        &self,
        _request: Request<ControllerGetCapabilitiesRequest>,
    ) -> Result<Response<ControllerGetCapabilitiesResponse>, Status> {
        Err(Status::unimplemented(
            "controller_get_capabilities is not implemented yet",
        ))
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
