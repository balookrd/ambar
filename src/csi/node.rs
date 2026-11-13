//! The CSI Node service: a placeholder, filled by Task 12.

use tonic::{Request, Response, Status};

use super::proto::node_server::Node;
use super::proto::{
    NodeExpandVolumeRequest, NodeExpandVolumeResponse, NodeGetCapabilitiesRequest, NodeGetCapabilitiesResponse,
    NodeGetInfoRequest, NodeGetInfoResponse, NodeGetStorageHealthRequest, NodeGetStorageHealthResponse,
    NodeGetVolumeHealthRequest, NodeGetVolumeHealthResponse, NodeGetVolumeStatsRequest, NodeGetVolumeStatsResponse,
    NodePublishVolumeRequest, NodePublishVolumeResponse, NodeStageVolumeRequest, NodeStageVolumeResponse,
    NodeUnpublishVolumeRequest, NodeUnpublishVolumeResponse, NodeUnstageVolumeRequest, NodeUnstageVolumeResponse,
};

/// Every call returns `UNIMPLEMENTED` until Task 12.
#[derive(Debug)]
pub struct NodeService;

#[tonic::async_trait]
impl Node for NodeService {
    async fn node_stage_volume(
        &self,
        _request: Request<NodeStageVolumeRequest>,
    ) -> Result<Response<NodeStageVolumeResponse>, Status> {
        Err(Status::unimplemented("node_stage_volume is not implemented yet"))
    }

    async fn node_unstage_volume(
        &self,
        _request: Request<NodeUnstageVolumeRequest>,
    ) -> Result<Response<NodeUnstageVolumeResponse>, Status> {
        Err(Status::unimplemented("node_unstage_volume is not implemented yet"))
    }

    async fn node_publish_volume(
        &self,
        _request: Request<NodePublishVolumeRequest>,
    ) -> Result<Response<NodePublishVolumeResponse>, Status> {
        Err(Status::unimplemented("node_publish_volume is not implemented yet"))
    }

    async fn node_unpublish_volume(
        &self,
        _request: Request<NodeUnpublishVolumeRequest>,
    ) -> Result<Response<NodeUnpublishVolumeResponse>, Status> {
        Err(Status::unimplemented("node_unpublish_volume is not implemented yet"))
    }

    async fn node_get_volume_stats(
        &self,
        _request: Request<NodeGetVolumeStatsRequest>,
    ) -> Result<Response<NodeGetVolumeStatsResponse>, Status> {
        Err(Status::unimplemented("node_get_volume_stats is not implemented yet"))
    }

    async fn node_get_volume_health(
        &self,
        _request: Request<NodeGetVolumeHealthRequest>,
    ) -> Result<Response<NodeGetVolumeHealthResponse>, Status> {
        Err(Status::unimplemented("node_get_volume_health is not implemented yet"))
    }

    async fn node_get_storage_health(
        &self,
        _request: Request<NodeGetStorageHealthRequest>,
    ) -> Result<Response<NodeGetStorageHealthResponse>, Status> {
        Err(Status::unimplemented("node_get_storage_health is not implemented yet"))
    }

    async fn node_expand_volume(
        &self,
        _request: Request<NodeExpandVolumeRequest>,
    ) -> Result<Response<NodeExpandVolumeResponse>, Status> {
        Err(Status::unimplemented("node_expand_volume is not implemented yet"))
    }

    async fn node_get_capabilities(
        &self,
        _request: Request<NodeGetCapabilitiesRequest>,
    ) -> Result<Response<NodeGetCapabilitiesResponse>, Status> {
        Err(Status::unimplemented("node_get_capabilities is not implemented yet"))
    }

    async fn node_get_info(
        &self,
        _request: Request<NodeGetInfoRequest>,
    ) -> Result<Response<NodeGetInfoResponse>, Status> {
        Err(Status::unimplemented("node_get_info is not implemented yet"))
    }
}
