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
use super::proto::{
    GetCapacityRequest, GetPluginInfoRequest, NodeGetInfoRequest, NodeGetVolumeStatsRequest, ProbeRequest,
};
use super::*;
use crate::agent::run::{Agent, VolumeLocks};
use crate::exec::Output;
use crate::paths::Paths;
use crate::tests::FakeRunner;
use crate::tests::volumes::{FakeNodes, FakeVolumes, cluster};

fn services(ready: bool) -> Services {
    let volumes = Arc::new(FakeVolumes::default());
    let nodes = Arc::new(FakeNodes(Ok(cluster(&[1 << 40; 3]))));
    let agent = Arc::new(Agent {
        node: "n1".to_string(),
        paths: Paths::default(),
        runner: Arc::new(FakeRunner::new(|_, _| Output {
            code: 0,
            stdout: String::new(),
            stderr: String::new(),
        })),
        volumes: volumes.clone(),
        locks: VolumeLocks::default(),
        ready: Arc::new(AtomicBool::new(ready)),
        options: Default::default(),
        metrics: Default::default(),
        is_tainted: Arc::new(AtomicBool::new(false)),
        staging: Default::default(),
    });
    Services {
        identity: IdentityService {
            ready: agent.ready.clone(),
        },
        controller: ControllerService::new(volumes, nodes.clone()),
        node: NodeService::new(agent, nodes),
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
async fn calls_that_are_not_implemented_say_so_and_the_node_service_answers() {
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
        .node_get_volume_stats(NodeGetVolumeStatsRequest::default())
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Unimplemented);
    let info = NodeClient::new(channel.clone())
        .node_get_info(NodeGetInfoRequest {})
        .await
        .unwrap()
        .into_inner();
    assert_eq!(info.node_id, "n1");
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

fn staging_dirs(parent: &Path) -> Vec<String> {
    fs::read_dir(parent)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".csi-"))
        .collect()
}

#[tokio::test]
async fn a_socket_replaced_by_another_instance_survives_the_shutdown() {
    let dir = short_dir();
    let socket = dir.path().join("csi.sock");
    let shutdown = CancellationToken::new();
    let server = tokio::spawn({
        let socket = socket.clone();
        let shutdown = shutdown.clone();
        async move { serve(&socket, services(true), shutdown).await }
    });
    wait_for_socket(&socket).await;

    // The new pod of a rolling update renames its socket over the path while
    // the old process is still alive.
    let other = dir.path().join("other.sock");
    let _listener = tokio::net::UnixListener::bind(&other).unwrap();
    fs::rename(&other, &socket).unwrap();

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("serve did not return after the shutdown")
        .unwrap()
        .unwrap();
    let meta = fs::symlink_metadata(&socket).expect("the replacing socket was removed");
    assert!(std::os::unix::fs::FileTypeExt::is_socket(&meta.file_type()));
    UnixStream::connect(&socket)
        .await
        .expect("the replacing socket no longer accepts connections");
}

#[tokio::test]
async fn an_existing_socket_is_replaced_and_the_staging_directory_does_not_linger() {
    let dir = short_dir();
    let socket = dir.path().join("csi.sock");
    drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
    let shutdown = CancellationToken::new();
    let server = tokio::spawn({
        let socket = socket.clone();
        let shutdown = shutdown.clone();
        async move { serve(&socket, services(true), shutdown).await }
    });
    wait_for_socket(&socket).await;
    assert_eq!(staging_dirs(dir.path()), Vec::<String>::new(), "while serving");

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("serve did not return after the shutdown")
        .unwrap()
        .unwrap();
    assert!(!socket.exists(), "the own socket is removed on shutdown");
    assert_eq!(staging_dirs(dir.path()), Vec::<String>::new(), "after the return");
}

#[tokio::test]
async fn serve_returns_promptly_without_open_connections() {
    let dir = short_dir();
    let socket = dir.path().join("csi.sock");
    let shutdown = CancellationToken::new();
    let server = tokio::spawn({
        let socket = socket.clone();
        let shutdown = shutdown.clone();
        async move { serve(&socket, services(true), shutdown).await }
    });
    wait_for_socket(&socket).await;

    let started = std::time::Instant::now();
    shutdown.cancel();
    server.await.unwrap().unwrap();
    assert!(
        started.elapsed() < SHUTDOWN_GRACE / 2,
        "an idle server took {:?} to stop",
        started.elapsed()
    );
}

#[tokio::test]
async fn serve_returns_within_the_cap_with_a_connection_held_open() {
    let dir = short_dir();
    let socket = dir.path().join("csi.sock");
    let shutdown = CancellationToken::new();
    let server = tokio::spawn({
        let socket = socket.clone();
        let shutdown = shutdown.clone();
        async move { serve(&socket, services(true), shutdown).await }
    });
    wait_for_socket(&socket).await;
    let channel = connect(&socket).await;
    let mut client = IdentityClient::new(channel);
    client.probe(ProbeRequest {}).await.unwrap();

    let started = std::time::Instant::now();
    shutdown.cancel();
    tokio::time::timeout(SHUTDOWN_GRACE + Duration::from_secs(3), server)
        .await
        .expect("serve outlived the shutdown cap")
        .unwrap()
        .unwrap();
    assert!(started.elapsed() < SHUTDOWN_GRACE + Duration::from_secs(3));
    drop(client);
}
