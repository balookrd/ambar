//! The CSI gRPC server: Identity, Controller (`CreateVolume`, `DeleteVolume`)
//! and Node (stage, publish, unpublish, unstage). Calls the services do not
//! implement return `UNIMPLEMENTED`.
//!
//! The server listens on a unix socket in kubelet's plugin directory. `serve`
//! creates the parent directory, renames a freshly bound socket (mode 0600)
//! over the path (replacing a leftover socket, refusing anything else) and on
//! shutdown removes the file only if it is still the socket this process made.

use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
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

/// Upper bound on how long `serve` waits after the shutdown was requested.
/// A kubelet keeps a long-lived connection, and the pod has 30 s to stop.
///
/// With the pinned tonic the cap does not cancel anything: after it expires
/// `serve` returns and the accept loop is dropped, but connection tasks and
/// in-flight handlers keep running until the process exits. Handlers of later
/// tasks must therefore be idempotent: a request can still complete after
/// `serve` has returned.
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

/// Binds the unix socket with mode 0600 (replacing a leftover socket at the
/// path) and serves until `shutdown`.
pub async fn serve(socket: &Path, services: Services, shutdown: CancellationToken) -> Result<(), CsiError> {
    let io_err = |e: io::Error| CsiError::Io(socket.to_path_buf(), e);
    let parent = socket
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(io_err)?;
    refuse_non_socket(socket)?;
    let (listener, identity) = bind_private(parent, socket).map_err(io_err)?;
    // From here the file is ours; it goes away on every way out unless another
    // instance has replaced it by then.
    let _guard = SocketFile { path: socket, identity };

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
/// reconcile loop creates files in the same process. `rename` replaces an
/// existing socket atomically. Returns the listener and the `(st_dev, st_ino)`
/// of the socket in place.
fn bind_private(parent: &Path, socket: &Path) -> io::Result<(UnixListener, (u64, u64))> {
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
    let meta = std::fs::symlink_metadata(socket)?;
    Ok((listener, (meta.dev(), meta.ino())))
}

/// A socket (leftover or not) at the path is replaced by the rename; a regular
/// file, a directory or a symlink is an error and stays untouched.
fn refuse_non_socket(path: &Path) -> Result<(), CsiError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => Ok(()),
        Ok(_) => Err(CsiError::NotASocket(path.to_path_buf())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(CsiError::Io(path.to_path_buf(), e)),
    }
}

/// Removes the socket file when dropped, but only if the path still holds the
/// socket with the recorded device and inode: during a rolling update the new
/// pod renames its socket over the path while this process is still shutting
/// down, and that socket must stay.
struct SocketFile<'a> {
    path: &'a Path,
    identity: (u64, u64),
}

impl Drop for SocketFile<'_> {
    fn drop(&mut self) {
        match std::fs::symlink_metadata(self.path) {
            Ok(meta) if meta.file_type().is_socket() && (meta.dev(), meta.ino()) == self.identity => {
                let _ = std::fs::remove_file(self.path);
            }
            _ => {}
        }
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
