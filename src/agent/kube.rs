//! The Kubernetes calls of the agent, behind the `Volumes` trait so that the
//! reconcile loop can run against a fake. `KubeVolumes` is the only
//! implementation; with `src/csi` it is the only code that talks to the API
//! server.
//!
//! API calls of `KubeVolumes` (kube 5.1.0, `Api<ReplicatedVolume>` over the
//! cluster-scoped resource):
//!
//! | Method | Call |
//! |---|---|
//! | `get` | `Api::get_opt` |
//! | `list` | the reflector cache once the watch has listed once, `Api::list` before that |
//! | `create` | `Api::create`; HTTP 409 is `Created::Exists` |
//! | `delete` | `Api::delete`; HTTP 404 is success |
//! | `edit` | `Api::get_opt`, the edit, `Api::replace` with the `resourceVersion` that was read; HTTP 409 reads again, up to `EDIT_ATTEMPTS` times |
//! | `apply_node_status` | `Api::patch_status` with `Patch::Apply` and `PatchParams::apply("ambar-<node>").force()`; HTTP 404 is success |
//! | `watch` | `watcher` (with the default backoff) through a `reflector` that fills the cache |

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream};
use kube::api::{Api, DeleteParams, ListParams, Patch, PatchParams, PostParams};
use kube::runtime::reflector::{self, Store};
use kube::runtime::watcher::{self, Event};
use kube::runtime::{WatchStreamExt, watcher as watcher_fn};
use kube::{Client, Resource};
use serde_json::json;

use crate::crd::{NodeStatus, ReplicatedVolume};

/// How often `edit` reads, edits and replaces an object that other writers
/// keep changing before it gives up with `ApiError::Conflict`.
pub const EDIT_ATTEMPTS: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiError {
    /// HTTP 409 on a write: the object changed since it was read.
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Created {
    New,
    /// An object with this name was there already; it was not changed.
    Exists,
}

/// Object edit with the signature `Volumes::edit` takes: `false` when no change is needed.
pub type Edit<'a> = &'a (dyn Fn(&mut ReplicatedVolume) -> bool + Send + Sync);

/// What a reconcile loop needs from the API server.
#[async_trait]
pub trait Volumes: Send + Sync {
    async fn get(&self, name: &str) -> Result<Option<ReplicatedVolume>, ApiError>;
    async fn list(&self) -> Result<Vec<ReplicatedVolume>, ApiError>;
    async fn create(&self, volume: &ReplicatedVolume) -> Result<Created, ApiError>;
    /// Not found is success.
    async fn delete(&self, name: &str) -> Result<(), ApiError>;
    /// Get, edit, replace with the `resourceVersion` that was read; read
    /// again and repeat on a conflict, `EDIT_ATTEMPTS` times in all.
    /// `Some(object)`: the edited object was written. `None`: the object does
    /// not exist, or `edit` returned `false` (nothing to change).
    async fn edit(&self, name: &str, edit: Edit<'_>) -> Result<Option<ReplicatedVolume>, ApiError>;
    /// Server-side apply of this node's entry of `status.nodes` under the
    /// field manager `ambar-<node>`; `None` removes the entry. An object that
    /// does not exist is success.
    async fn apply_node_status(&self, name: &str, node: &str, entry: Option<&NodeStatus>) -> Result<(), ApiError>;
    /// Names of the objects that were added, changed or deleted, starting
    /// with every existing object. Call once; the stream does not end.
    fn watch(&self) -> BoxStream<'static, Result<String, ApiError>>;
}

/// Read and replace of one object: the two calls `edit_retrying` repeats.
#[async_trait]
pub trait Objects: Send + Sync {
    async fn read(&self, name: &str) -> Result<Option<ReplicatedVolume>, ApiError>;
    /// Replaces the object; `volume.metadata.resource_version` is the version
    /// that was read, a newer one in the store is `ApiError::Conflict`.
    async fn replace(&self, name: &str, volume: &ReplicatedVolume) -> Result<ReplicatedVolume, ApiError>;
}

/// The `Volumes::edit` contract over `Objects`.
pub async fn edit_retrying(
    objects: &dyn Objects,
    name: &str,
    edit: Edit<'_>,
) -> Result<Option<ReplicatedVolume>, ApiError> {
    let mut last = ApiError::Conflict("no attempt was made".to_string());
    for _ in 0..EDIT_ATTEMPTS {
        let Some(mut volume) = objects.read(name).await? else {
            return Ok(None);
        };
        if !edit(&mut volume) {
            return Ok(None);
        }
        match objects.replace(name, &volume).await {
            Ok(written) => return Ok(Some(written)),
            Err(conflict @ ApiError::Conflict(_)) => last = conflict,
            Err(other) => return Err(other),
        }
    }
    Err(last)
}

fn code_of(error: &kube::Error) -> Option<u16> {
    match error {
        kube::Error::Api(status) => Some(status.code),
        _ => None,
    }
}

fn api_error(error: kube::Error) -> ApiError {
    match code_of(&error) {
        Some(409) => ApiError::Conflict(error.to_string()),
        _ => ApiError::Other(error.to_string()),
    }
}

pub struct KubeVolumes {
    api: Api<ReplicatedVolume>,
    cache: Store<ReplicatedVolume>,
    /// Taken by the first `watch`; the reflector owns it from then on.
    writer: Mutex<Option<reflector::store::Writer<ReplicatedVolume>>>,
    /// The watch has listed every object once: the cache is complete.
    listed: Arc<AtomicBool>,
}

impl KubeVolumes {
    pub fn new(client: Client) -> Self {
        let (cache, writer) = reflector::store();
        Self {
            api: Api::all(client),
            cache,
            writer: Mutex::new(Some(writer)),
            listed: Arc::new(AtomicBool::new(false)),
        }
    }
}

#[async_trait]
impl Objects for KubeVolumes {
    async fn read(&self, name: &str) -> Result<Option<ReplicatedVolume>, ApiError> {
        self.get(name).await
    }

    async fn replace(&self, name: &str, volume: &ReplicatedVolume) -> Result<ReplicatedVolume, ApiError> {
        self.api
            .replace(name, &PostParams::default(), volume)
            .await
            .map_err(api_error)
    }
}

#[async_trait]
impl Volumes for KubeVolumes {
    async fn get(&self, name: &str) -> Result<Option<ReplicatedVolume>, ApiError> {
        self.api.get_opt(name).await.map_err(api_error)
    }

    async fn list(&self) -> Result<Vec<ReplicatedVolume>, ApiError> {
        if self.listed.load(Ordering::SeqCst) {
            return Ok(self.cache.state().iter().map(|volume| (**volume).clone()).collect());
        }
        let list = self.api.list(&ListParams::default()).await.map_err(api_error)?;
        Ok(list.items)
    }

    async fn create(&self, volume: &ReplicatedVolume) -> Result<Created, ApiError> {
        match self.api.create(&PostParams::default(), volume).await {
            Ok(_) => Ok(Created::New),
            Err(e) if code_of(&e) == Some(409) => Ok(Created::Exists),
            Err(e) => Err(api_error(e)),
        }
    }

    async fn delete(&self, name: &str) -> Result<(), ApiError> {
        match self.api.delete(name, &DeleteParams::default()).await {
            Ok(_) => Ok(()),
            Err(e) if code_of(&e) == Some(404) => Ok(()),
            Err(e) => Err(api_error(e)),
        }
    }

    async fn edit(&self, name: &str, edit: Edit<'_>) -> Result<Option<ReplicatedVolume>, ApiError> {
        edit_retrying(self, name, edit).await
    }

    async fn apply_node_status(&self, name: &str, node: &str, entry: Option<&NodeStatus>) -> Result<(), ApiError> {
        // Only `status.nodes` is in the patch. The CRD declares the list as a
        // map keyed by `name`, so the entries of the other nodes belong to
        // their own field managers and survive; an empty list removes the
        // entry this manager applied before.
        let patch = json!({
            "apiVersion": ReplicatedVolume::api_version(&()),
            "kind": ReplicatedVolume::kind(&()),
            "status": { "nodes": entry.into_iter().collect::<Vec<_>>() },
        });
        let params = PatchParams::apply(&format!("ambar-{node}")).force();
        match self.api.patch_status(name, &params, &Patch::Apply(&patch)).await {
            Ok(_) => Ok(()),
            Err(e) if code_of(&e) == Some(404) => Ok(()),
            Err(e) => Err(api_error(e)),
        }
    }

    fn watch(&self) -> BoxStream<'static, Result<String, ApiError>> {
        let Some(writer) = self.writer.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            return stream::empty().boxed();
        };
        let listed = self.listed.clone();
        let events = watcher_fn(self.api.clone(), watcher::Config::default()).default_backoff();
        reflector::reflector(writer, events)
            .filter_map(move |item| {
                let listed = listed.clone();
                async move {
                    match item {
                        Ok(Event::Apply(volume) | Event::Delete(volume) | Event::InitApply(volume)) => {
                            volume.metadata.name.map(Ok)
                        }
                        Ok(Event::InitDone) => {
                            // The reflector has applied this event: the cache
                            // now holds every object the list returned.
                            listed.store(true, Ordering::SeqCst);
                            None
                        }
                        Ok(Event::Init) => None,
                        Err(e) => Some(Err(ApiError::Other(format!("watch: {e}")))),
                    }
                }
            })
            .boxed()
    }
}
