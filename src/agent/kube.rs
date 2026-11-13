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
//! | `list_fresh` | `Api::list` with `ListParams::default()` (no `resourceVersion`: a list without it is a consistent read), following `metadata.continue` while the server sets it |
//! | `create` | `Api::create`; the uid of the stored object is in `Created::New`; HTTP 409 is `Created::Exists` |
//! | `delete` | `Api::delete`; HTTP 404 is success |
//! | `delete_exact` | `Api::delete` with `DeleteParams::preconditions` (`uid`); HTTP 404 is success, a failed precondition (HTTP 409) is `ApiError::Conflict` |
//! | `remove_finalizer`, `mark_initial_sync_done` | `Api::get_opt`, then `Api::patch` with `Patch::Json`: `test /metadata/uid`, a `test` of the value that is edited, the edit; a refused patch (HTTP 409 or 422) reads again, up to `EDIT_ATTEMPTS` times |
//! | `mark_formatted` | `Api::get_opt`, then `Api::patch` with `Patch::Json`: `test /metadata/uid`, `add` of the annotation `FORMATTED_ANNOTATION` (the whole `/metadata/annotations` map, guarded by a `test` of `/metadata/resourceVersion`, when the object has none); retried like the edits above |
//! | `replace_members` | `Api::get_opt`, then `Api::patch` with `Patch::Json`: `test /metadata/uid`, `test /spec/nodes` (the list the caller computed its edit from), `replace /spec/nodes`; retried like the edits above |
//! | `apply_node_status` | `Api::patch_status` with `Patch::Apply` and `PatchParams::apply("ambar-<node>").force()`; HTTP 404 is success |
//! | `remove_node_entry` | `Api::get_opt`, then `Api::patch_status` with `Patch::Json`: `test /metadata/uid`, `test /status/nodes/<i>/name`, `remove /status/nodes/<i>`; retried like the two edits above |
//! | `watch` | `watcher` (with the default backoff) through a `reflector` that fills the cache |
//!
//! Every call of the table except `watch` runs under `API_CALL_TIMEOUT` (`bounded`); an
//! expired call is an `ApiError::Other`, and the callers treat it like any other API error.
//!
//! Why a JSON patch for the two edits of the object itself (`metadata.finalizers`
//! and `spec.initialSync`). The API server applies the patch to the stored
//! object, so fields this build does not know are not read and not written
//! back (a get, edit, replace of a typed object drops them). The `test`
//! operations are the preconditions: `/metadata/uid` ties the edit to the
//! object whose uid the pass saw (an object that was deleted and created again
//! under the same name has another uid and is not edited), and a `test` of the
//! value being changed makes the edit apply only to the state it was computed
//! from. `/metadata/resourceVersion` is not tested: every status write of
//! every node changes it, and the edits do not depend on anything else.
//! A failed `test` is HTTP 422 (checked on the stand); the edit is computed
//! again from a fresh read.
//!
//! Why `remove_node_entry` exists next to `apply_node_status(.., None)`.
//! Server-side apply of an empty list removes only the fields this field
//! manager owns. When another manager co-owns the entry (an operator's
//! `kubectl patch` or `kubectl edit` of the status), the entry stays after the
//! empty apply, and the other nodes keep waiting for it. The agent reads the
//! object after the empty apply and, when its entry is still there, removes it
//! with this patch.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream};
use kube::api::{Api, DeleteParams, ListParams, Patch, PatchParams, PostParams, Preconditions};
use kube::runtime::reflector::{self, Store};
use kube::runtime::watcher::{self, Event};
use kube::runtime::{WatchStreamExt, watcher as watcher_fn};
use kube::{Client, Resource};
use serde_json::{Value, json};

use crate::crd::{MemberNode, NodeStatus, ReplicatedVolume};
use crate::model::InitialSync;

/// Annotation the CSI Node service sets on an object after it has made (or
/// found) the filesystem on the volume. Its absence is one of the conditions
/// for `mkfs`; the agent never removes it.
pub const FORMATTED_ANNOTATION: &str = "ambar.balookrd.github.io/formatted";

/// How often an edit of the object reads it and patches it again after the
/// API server refused the patch, before it gives up.
pub const EDIT_ATTEMPTS: usize = 5;

/// The longest a call to the API server (the watch stream excepted) may take.
/// `kube::Client` has no timeout of its own: without this limit a request
/// that an API server never answers would keep its caller, and the lock the
/// caller holds, forever.
pub const API_CALL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiError {
    /// HTTP 409 on a write: the object changed since it was read.
    #[error("conflict: {0}")]
    Conflict(String),
    /// HTTP 422 on a patch: a `test` operation failed, or the result is not valid.
    #[error("rejected: {0}")]
    Rejected(String),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Created {
    /// The object was stored; the uid the API server gave it.
    New(String),
    /// An object with this name was there already; it was not changed.
    Exists,
}

/// What a reconcile loop needs from the API server.
#[async_trait]
pub trait Volumes: Send + Sync {
    async fn get(&self, name: &str) -> Result<Option<ReplicatedVolume>, ApiError>;
    async fn list(&self) -> Result<Vec<ReplicatedVolume>, ApiError>;
    /// Every object as the API server holds it now: a live LIST, never the
    /// watch cache. A caller that decides from the list what is free (the
    /// placement of a new volume) needs this one: the cache lags behind a
    /// create by the watch latency.
    async fn list_fresh(&self) -> Result<Vec<ReplicatedVolume>, ApiError>;
    async fn create(&self, volume: &ReplicatedVolume) -> Result<Created, ApiError>;
    /// Not found is success.
    async fn delete(&self, name: &str) -> Result<(), ApiError>;
    /// Deletes the object `name` only while it has the uid `uid`; an object
    /// that was created again under the name is not touched. Not found is
    /// success, another uid is `ApiError::Conflict`.
    async fn delete_exact(&self, name: &str, uid: &str) -> Result<(), ApiError>;
    /// Removes `finalizer` from the object `name`, if the object has the uid
    /// `uid`. `true`: the finalizer was removed. `false`: the object does not
    /// exist, has another uid (it was created again), or does not carry the
    /// finalizer; nothing was changed.
    async fn remove_finalizer(&self, name: &str, uid: &str, finalizer: &str) -> Result<bool, ApiError>;
    /// Sets `spec.initialSync` from `Skip` to `Done` on the object `name`, if
    /// the object has the uid `uid`. `false`: nothing was changed (no object,
    /// another uid, or the field is not `Skip`).
    async fn mark_initial_sync_done(&self, name: &str, uid: &str) -> Result<bool, ApiError>;
    /// Sets the annotation `FORMATTED_ANNOTATION` to `"true"` on the object
    /// `name`, if the object has the uid `uid`. `false`: nothing was changed
    /// (no object, another uid, or the annotation is there already).
    async fn mark_formatted(&self, name: &str, uid: &str) -> Result<bool, ApiError>;
    /// Replaces `spec.nodes` of the object `name` with `members`, if the
    /// object has the uid `uid` and its `spec.nodes` is still `expected`, the
    /// list the caller computed `members` from. `false`: nothing was changed
    /// (no object, another uid, or the list is not `expected` any more); the
    /// caller reads the object again and computes the edit again.
    async fn replace_members(
        &self,
        name: &str,
        uid: &str,
        expected: &[MemberNode],
        members: &[MemberNode],
    ) -> Result<bool, ApiError>;
    /// Server-side apply of this node's entry of `status.nodes` under the
    /// field manager `ambar-<node>`; `None` removes the entry. An object that
    /// does not exist is success.
    async fn apply_node_status(&self, name: &str, node: &str, entry: Option<&NodeStatus>) -> Result<(), ApiError>;
    /// Removes the entry of `node` from `status.nodes` of the object `name`
    /// with a JSON patch on the status subresource, if the object has the uid
    /// `uid`. `true`: the entry was removed. `false`: nothing was changed (no
    /// object, another uid, or no such entry).
    async fn remove_node_entry(&self, name: &str, uid: &str, node: &str) -> Result<bool, ApiError>;
    /// Names of the objects that were added, changed or deleted, starting
    /// with every existing object. Call once; the stream does not end.
    fn watch(&self) -> BoxStream<'static, Result<String, ApiError>>;
}

/// Read and patch of one object: the two calls `patch_retrying` repeats.
#[async_trait]
pub trait Objects: Send + Sync {
    async fn read(&self, name: &str) -> Result<Option<ReplicatedVolume>, ApiError>;
    /// Applies the JSON patch `ops` to the stored object. `Ok(false)`: the
    /// object does not exist. A failed `test` operation is
    /// `ApiError::Rejected`, an object that changed under the patch
    /// `ApiError::Conflict`.
    async fn patch(&self, name: &str, ops: Vec<Value>) -> Result<bool, ApiError>;
    /// The same on the status subresource.
    async fn patch_status(&self, name: &str, ops: Vec<Value>) -> Result<bool, ApiError>;
}

/// What a JSON patch is applied to.
#[derive(Clone, Copy)]
enum Target {
    Object,
    Status,
}

/// JSON-patch operations for one edit, computed from the object as read; `None`: nothing to change.
pub type Ops<'a> = &'a (dyn Fn(&ReplicatedVolume) -> Option<Vec<Value>> + Send + Sync);

/// Reads the object, computes the edit, patches with `test /metadata/uid`
/// first. `Ok(false)` when the object is gone, has another uid than `uid` or
/// needs no change. A patch the API server refuses (a failed `test`, a
/// conflict) is computed again from a fresh read, `EDIT_ATTEMPTS` times in all.
pub async fn patch_retrying(objects: &dyn Objects, name: &str, uid: &str, ops: Ops<'_>) -> Result<bool, ApiError> {
    edit_retrying(objects, name, uid, ops, Target::Object).await
}

/// `patch_retrying` for the status subresource.
pub async fn patch_status_retrying(
    objects: &dyn Objects,
    name: &str,
    uid: &str,
    ops: Ops<'_>,
) -> Result<bool, ApiError> {
    edit_retrying(objects, name, uid, ops, Target::Status).await
}

async fn edit_retrying(
    objects: &dyn Objects,
    name: &str,
    uid: &str,
    ops: Ops<'_>,
    target: Target,
) -> Result<bool, ApiError> {
    let mut last = ApiError::Conflict("no attempt was made".to_string());
    for _ in 0..EDIT_ATTEMPTS {
        let Some(current) = objects.read(name).await? else {
            return Ok(false);
        };
        if current.metadata.uid.as_deref() != Some(uid) {
            return Ok(false);
        }
        let Some(edit) = ops(&current) else {
            return Ok(false);
        };
        let mut patch = vec![json!({"op": "test", "path": "/metadata/uid", "value": uid})];
        patch.extend(edit);
        let result = match target {
            Target::Object => objects.patch(name, patch).await,
            Target::Status => objects.patch_status(name, patch).await,
        };
        match result {
            Ok(patched) => return Ok(patched),
            Err(refused @ (ApiError::Conflict(_) | ApiError::Rejected(_))) => last = refused,
            Err(other) => return Err(other),
        }
    }
    Err(last)
}

/// Operations that remove `finalizer`, applying only while it is at the index
/// it has in `object`.
pub fn remove_finalizer_ops(object: &ReplicatedVolume, finalizer: &str) -> Option<Vec<Value>> {
    let index = object
        .metadata
        .finalizers
        .as_ref()?
        .iter()
        .position(|f| f == finalizer)?;
    let path = format!("/metadata/finalizers/{index}");
    Some(vec![
        json!({"op": "test", "path": path, "value": finalizer}),
        json!({"op": "remove", "path": path}),
    ])
}

/// Operations that remove the entry of `node` from `status.nodes`, applying
/// only while that entry is at the index it has in `object`.
pub fn remove_node_entry_ops(object: &ReplicatedVolume, node: &str) -> Option<Vec<Value>> {
    let index = object.status.as_ref()?.nodes.iter().position(|n| n.name == node)?;
    let path = format!("/status/nodes/{index}");
    Some(vec![
        json!({"op": "test", "path": format!("{path}/name"), "value": node}),
        json!({"op": "remove", "path": path}),
    ])
}

/// Operations that change `spec.initialSync` from `Skip` to `Done`.
pub fn initial_sync_done_ops(object: &ReplicatedVolume) -> Option<Vec<Value>> {
    if object.spec.initial_sync != InitialSync::Skip {
        return None;
    }
    let skip = serde_json::to_value(InitialSync::Skip).ok()?;
    let done = serde_json::to_value(InitialSync::Done).ok()?;
    Some(vec![
        json!({"op": "test", "path": "/spec/initialSync", "value": skip}),
        json!({"op": "replace", "path": "/spec/initialSync", "value": done}),
    ])
}

/// Operations that set `FORMATTED_ANNOTATION`; `None` when it is set already.
/// Without any annotation the operation adds the whole map, which would erase
/// annotations written in between, so it applies only to the resource version
/// it was computed from.
pub fn mark_formatted_ops(object: &ReplicatedVolume) -> Option<Vec<Value>> {
    let annotations = object.metadata.annotations.as_ref();
    if annotations.is_some_and(|a| a.contains_key(FORMATTED_ANNOTATION)) {
        return None;
    }
    if annotations.is_some() {
        // JSON-pointer escaping of the `/` in the key.
        let path = format!("/metadata/annotations/{}", FORMATTED_ANNOTATION.replace('/', "~1"));
        return Some(vec![json!({"op": "add", "path": path, "value": "true"})]);
    }
    let mut ops = Vec::new();
    if let Some(version) = &object.metadata.resource_version {
        ops.push(json!({"op": "test", "path": "/metadata/resourceVersion", "value": version}));
    }
    ops.push(json!({"op": "add", "path": "/metadata/annotations", "value": {FORMATTED_ANNOTATION: "true"}}));
    Some(ops)
}

/// Operations that replace `spec.nodes` with `members`, applying only while
/// the list is `expected`; `None` when `object` has another list.
pub fn replace_members_ops(
    object: &ReplicatedVolume,
    expected: &[MemberNode],
    members: &[MemberNode],
) -> Option<Vec<Value>> {
    if object.spec.nodes != expected {
        return None;
    }
    Some(vec![
        json!({"op": "test", "path": "/spec/nodes", "value": expected}),
        json!({"op": "replace", "path": "/spec/nodes", "value": members}),
    ])
}

/// Runs one API call under `API_CALL_TIMEOUT`. The outer `Err` is the expiry,
/// an `ApiError::Other`: callers end it in their retry paths (reconcile: the
/// name is due again after `ERROR_DELAY`; CSI: `UNAVAILABLE`). The inner
/// result is the call's own, for callers that handle HTTP codes themselves.
pub(crate) async fn bounded<T>(
    call: impl Future<Output = Result<T, kube::Error>>,
) -> Result<Result<T, kube::Error>, ApiError> {
    tokio::time::timeout(API_CALL_TIMEOUT, call).await.map_err(|_| {
        ApiError::Other(format!(
            "the API server did not answer within {} s",
            API_CALL_TIMEOUT.as_secs()
        ))
    })
}

/// `bounded` for a call whose errors need no special handling.
async fn call<T>(call: impl Future<Output = Result<T, kube::Error>>) -> Result<T, ApiError> {
    bounded(call).await?.map_err(api_error)
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
        Some(422) => ApiError::Rejected(error.to_string()),
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

    async fn patch(&self, name: &str, ops: Vec<Value>) -> Result<bool, ApiError> {
        // `json_patch::Patch` is not named here: the operations are parsed into
        // whatever `Patch::Json` takes.
        let ops = serde_json::from_value(Value::Array(ops)).map_err(|e| ApiError::Other(e.to_string()))?;
        match bounded(self.api.patch(name, &PatchParams::default(), &Patch::Json::<()>(ops))).await? {
            Ok(_) => Ok(true),
            Err(e) if code_of(&e) == Some(404) => Ok(false),
            Err(e) => Err(api_error(e)),
        }
    }

    async fn patch_status(&self, name: &str, ops: Vec<Value>) -> Result<bool, ApiError> {
        let ops = serde_json::from_value(Value::Array(ops)).map_err(|e| ApiError::Other(e.to_string()))?;
        match bounded(
            self.api
                .patch_status(name, &PatchParams::default(), &Patch::Json::<()>(ops)),
        )
        .await?
        {
            Ok(_) => Ok(true),
            Err(e) if code_of(&e) == Some(404) => Ok(false),
            Err(e) => Err(api_error(e)),
        }
    }
}

#[async_trait]
impl Volumes for KubeVolumes {
    async fn get(&self, name: &str) -> Result<Option<ReplicatedVolume>, ApiError> {
        call(self.api.get_opt(name)).await
    }

    async fn list(&self) -> Result<Vec<ReplicatedVolume>, ApiError> {
        if self.listed.load(Ordering::SeqCst) {
            return Ok(self.cache.state().iter().map(|volume| (**volume).clone()).collect());
        }
        let list = call(self.api.list(&ListParams::default())).await?;
        Ok(list.items)
    }

    async fn list_fresh(&self) -> Result<Vec<ReplicatedVolume>, ApiError> {
        // Default parameters: no `resourceVersion` (a list without it is a consistent read) and no
        // `limit` (the server returns everything in one response). The
        // client does not paginate by itself; a `continue` token in the
        // answer is followed all the same, so that a server that chunks
        // anyway cannot hide objects.
        let mut params = ListParams::default();
        let mut items = Vec::new();
        loop {
            let page = call(self.api.list(&params)).await?;
            items.extend(page.items);
            match page.metadata.continue_ {
                Some(token) if !token.is_empty() => params.continue_token = Some(token),
                _ => return Ok(items),
            }
        }
    }

    async fn create(&self, volume: &ReplicatedVolume) -> Result<Created, ApiError> {
        match bounded(self.api.create(&PostParams::default(), volume)).await? {
            Ok(stored) => match stored.metadata.uid {
                Some(uid) => Ok(Created::New(uid)),
                None => Err(ApiError::Other(
                    "the API server stored the object without a uid".to_string(),
                )),
            },
            Err(e) if code_of(&e) == Some(409) => Ok(Created::Exists),
            Err(e) => Err(api_error(e)),
        }
    }

    async fn delete(&self, name: &str) -> Result<(), ApiError> {
        match bounded(self.api.delete(name, &DeleteParams::default())).await? {
            Ok(_) => Ok(()),
            Err(e) if code_of(&e) == Some(404) => Ok(()),
            Err(e) => Err(api_error(e)),
        }
    }

    async fn delete_exact(&self, name: &str, uid: &str) -> Result<(), ApiError> {
        let params = DeleteParams::default().preconditions(Preconditions {
            uid: Some(uid.to_string()),
            resource_version: None,
        });
        match bounded(self.api.delete(name, &params)).await? {
            Ok(_) => Ok(()),
            Err(e) if code_of(&e) == Some(404) => Ok(()),
            Err(e) => Err(api_error(e)),
        }
    }

    async fn remove_finalizer(&self, name: &str, uid: &str, finalizer: &str) -> Result<bool, ApiError> {
        patch_retrying(self, name, uid, &|object| remove_finalizer_ops(object, finalizer)).await
    }

    async fn mark_initial_sync_done(&self, name: &str, uid: &str) -> Result<bool, ApiError> {
        patch_retrying(self, name, uid, &initial_sync_done_ops).await
    }

    async fn mark_formatted(&self, name: &str, uid: &str) -> Result<bool, ApiError> {
        patch_retrying(self, name, uid, &mark_formatted_ops).await
    }

    async fn replace_members(
        &self,
        name: &str,
        uid: &str,
        expected: &[MemberNode],
        members: &[MemberNode],
    ) -> Result<bool, ApiError> {
        patch_retrying(self, name, uid, &|object| {
            replace_members_ops(object, expected, members)
        })
        .await
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
        match bounded(self.api.patch_status(name, &params, &Patch::Apply(&patch))).await? {
            Ok(_) => Ok(()),
            Err(e) if code_of(&e) == Some(404) => Ok(()),
            Err(e) => Err(api_error(e)),
        }
    }

    async fn remove_node_entry(&self, name: &str, uid: &str, node: &str) -> Result<bool, ApiError> {
        patch_status_retrying(self, name, uid, &|object| remove_node_entry_ops(object, node)).await
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

#[cfg(test)]
#[path = "tests/kube.rs"]
mod tests;
