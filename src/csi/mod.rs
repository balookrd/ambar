//! The CSI gRPC server: the Identity service answers; Controller and Node are
//! placeholders that return `UNIMPLEMENTED` until Tasks 11 and 12 fill them.
//!
//! The server listens on a unix socket in kubelet's plugin directory. `serve`
//! creates the parent directory, removes a stale socket file (and nothing else
//! at that path), binds with mode 0600 and removes the file on shutdown.

use std::io;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use thiserror::Error;
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;
use tokio_util::sync::CancellationToken;
use tonic::transport::Server;

pub mod controller;
pub mod identity;
pub mod node;

pub use controller::ControllerService;
pub use identity::IdentityService;
pub use node::NodeService;

/// Types and services generated from `proto/csi.proto` (package `csi.v1`).
pub mod proto {
    tonic::include_proto!("csi.v1");
}

/// How long open connections get to finish after the shutdown was requested.
/// A kubelet keeps a long-lived connection, and the pod has 30 s to stop.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, Error)]
pub enum CsiError {
    #[error("{path}: {source}", path = .0.display(), source = .1)]
    Io(PathBuf, #[source] io::Error),
    #[error("{} exists and is not a socket; it is left in place", .0.display())]
    NotASocket(PathBuf),
    #[error("the CSI server failed: {0}")]
    Serve(#[source] tonic::transport::Error),
}

pub struct Services {
    pub identity: IdentityService,
    pub controller: ControllerService,
    pub node: NodeService,
}

/// Removes a stale socket file, binds the unix socket with mode 0600 and
/// serves until `shutdown`.
pub async fn serve(socket: &Path, services: Services, shutdown: CancellationToken) -> Result<(), CsiError> {
    let io_err = |e: io::Error| CsiError::Io(socket.to_path_buf(), e);
    let parent = socket
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(io_err)?;
    remove_stale_socket(socket)?;
    let listener = bind_private(parent, socket).map_err(io_err)?;
    // From here the file is ours, and it goes away on every way out.
    let _guard = SocketFile(socket);

    let server = Server::builder()
        .add_service(proto::identity_server::IdentityServer::new(services.identity))
        .add_service(proto::controller_server::ControllerServer::new(services.controller))
        .add_service(proto::node_server::NodeServer::new(services.node))
        .serve_with_incoming_shutdown(UnixListenerStream::new(listener), shutdown.clone().cancelled_owned());
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => result.map_err(CsiError::Serve),
        () = async {
            shutdown.cancelled().await;
            tokio::time::sleep(SHUTDOWN_GRACE).await;
        } => Ok(()),
    }
}

/// Binds the socket in a private (0700) staging directory next to `socket`,
/// sets mode 0600 and renames it into place, so that no other user can connect
/// in the time between `bind` (which creates the file with the umask of the
/// process) and `chmod`. `umask` is not used: it is process-wide, and the
/// reconcile loop creates files in the same process.
fn bind_private(parent: &Path, socket: &Path) -> io::Result<UnixListener> {
    let staging = parent.join(format!(".csi-{}", std::process::id()));
    match std::fs::remove_dir_all(&staging) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    std::fs::create_dir(&staging)?;
    let _staging = StagingDir(&staging);
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o700))?;
    let staged = staging.join("s");
    let listener = UnixListener::bind(&staged)?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&staged, socket)?;
    Ok(listener)
}

/// A leftover socket of a previous process is removed; a regular file, a
/// directory or a symlink at the path is an error and stays untouched.
fn remove_stale_socket(path: &Path) -> Result<(), CsiError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => {
            std::fs::remove_file(path).map_err(|e| CsiError::Io(path.to_path_buf(), e))
        }
        Ok(_) => Err(CsiError::NotASocket(path.to_path_buf())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(CsiError::Io(path.to_path_buf(), e)),
    }
}

/// Removes the socket file when dropped.
struct SocketFile<'a>(&'a Path);

impl Drop for SocketFile<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0);
    }
}

/// Removes the staging directory (and what is left in it) when dropped.
struct StagingDir<'a>(&'a Path);

impl Drop for StagingDir<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0);
    }
}

#[cfg(test)]
mod tests;
