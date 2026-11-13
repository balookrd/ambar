use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;
use tokio_util::sync::CancellationToken;
use tonic::Code;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

use super::proto::controller_client::ControllerClient;
use super::proto::identity_client::IdentityClient;
use super::proto::node_client::NodeClient;
use super::proto::{GetCapacityRequest, GetPluginInfoRequest, NodeGetInfoRequest, ProbeRequest};
use super::*;

fn services(ready: bool) -> Services {
    Services {
        identity: IdentityService {
            ready: Arc::new(AtomicBool::new(ready)),
        },
        controller: ControllerService,
        node: NodeService,
    }
}

/// The unix socket path must stay under ~104 bytes on macOS, so the temporary
/// directory has a short prefix and the socket sits one level below it.
fn short_dir() -> tempfile::TempDir {
    tempfile::Builder::new().prefix("amb").tempdir_in("/tmp").unwrap()
}

async fn connect(socket: &Path) -> Channel {
    let socket: PathBuf = socket.to_path_buf();
    // The URI is a placeholder: the connector ignores it.
    Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector(service_fn(move |_: Uri| {
            let socket = socket.clone();
            async move { Ok::<_, std::io::Error>(TokioIo::new(UnixStream::connect(socket).await?)) }
        }))
        .await
        .unwrap()
}

async fn wait_for_socket(socket: &Path) {
    for _ in 0..200 {
        // A stale socket file refuses connections; the new one accepts them.
        if UnixStream::connect(socket).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the socket {} did not accept connections", socket.display());
}

#[tokio::test]
async fn serves_identity_and_restarts_over_a_stale_socket() {
    let dir = short_dir();
    let socket = dir.path().join("plugin").join("csi.sock");

    for round in 0..2 {
        let shutdown = CancellationToken::new();
        let server = tokio::spawn({
            let socket = socket.clone();
            let shutdown = shutdown.clone();
            async move { serve(&socket, services(true), shutdown).await }
        });
        wait_for_socket(&socket).await;
        assert_eq!(fs::metadata(&socket).unwrap().permissions().mode() & 0o777, 0o600);

        let mut client = IdentityClient::new(connect(&socket).await);
        let info = client
            .get_plugin_info(GetPluginInfoRequest {})
            .await
            .unwrap()
            .into_inner();
        assert_eq!(info.name, "ambar.balookrd.github.io");
        let probe = client.probe(ProbeRequest {}).await.unwrap().into_inner();
        assert_eq!(probe.ready, Some(true));

        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(10), server)
            .await
            .expect("serve did not return after the shutdown")
            .unwrap()
            .unwrap();
        assert!(
            !socket.exists(),
            "the socket file is removed on a clean shutdown (round {round})"
        );

        if round == 0 {
            // A socket left by a crashed process: the next start removes it.
            drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
            assert!(socket.exists());
        }
    }
}

#[tokio::test]
async fn controller_and_node_are_unimplemented_for_now() {
    let dir = short_dir();
    let socket = dir.path().join("csi.sock");
    let shutdown = CancellationToken::new();
    let server = tokio::spawn({
        let socket = socket.clone();
        let shutdown = shutdown.clone();
        async move { serve(&socket, services(false), shutdown).await }
    });
    wait_for_socket(&socket).await;
    let channel = connect(&socket).await;

    let err = ControllerClient::new(channel.clone())
        .get_capacity(GetCapacityRequest::default())
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Unimplemented);
    let err = NodeClient::new(channel.clone())
        .node_get_info(NodeGetInfoRequest {})
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Unimplemented);
    let probe = IdentityClient::new(channel)
        .probe(ProbeRequest {})
        .await
        .unwrap()
        .into_inner();
    assert_eq!(probe.ready, Some(false));

    shutdown.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_regular_file_at_the_socket_path_is_not_removed() {
    let dir = short_dir();
    let socket = dir.path().join("csi.sock");
    fs::write(&socket, b"data").unwrap();

    let err = serve(&socket, services(true), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, CsiError::NotASocket(_)), "{err}");
    assert_eq!(fs::read(&socket).unwrap(), b"data");
}

#[tokio::test]
async fn a_directory_at_the_socket_path_is_not_removed() {
    let dir = short_dir();
    let socket = dir.path().join("csi.sock");
    fs::create_dir(&socket).unwrap();

    let err = serve(&socket, services(true), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, CsiError::NotASocket(_)), "{err}");
    assert!(socket.is_dir());
}

#[tokio::test]
async fn a_symlink_at_the_socket_path_is_not_followed() {
    let dir = short_dir();
    let target = dir.path().join("target");
    fs::write(&target, b"data").unwrap();
    let socket = dir.path().join("csi.sock");
    std::os::unix::fs::symlink(&target, &socket).unwrap();

    let err = serve(&socket, services(true), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, CsiError::NotASocket(_)), "{err}");
    assert_eq!(fs::read(&target).unwrap(), b"data");
}
