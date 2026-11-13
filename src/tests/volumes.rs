//! A fake API server for the tests: `Volumes` and `Objects` over a map of
//! objects, shared by the reconcile-loop tests and the CSI Controller tests.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::agent::kube::{
    ApiError, Created, Objects, Volumes, initial_sync_done_ops, patch_retrying, patch_status_retrying,
    remove_finalizer_ops, remove_node_entry_ops,
};
use crate::agent::nodes::NodeMap;
use crate::config::ClusterNodes;
use crate::crd::{NodeStatus, ReplicatedVolume};

// ---- fake API server ----

/// Objects (as JSON, so that fields this build does not know survive) in a
/// map and a log of the calls the agent made. `delete` follows the API
/// server: an object with finalizers gets a deletion timestamp, one without is
/// gone; removing the last finalizer of a deleting object removes it. `patch`
/// applies the JSON-patch operations `test`, `replace` and `remove`; a failed
/// `test` is `ApiError::Rejected`, as the API server's 422.
#[derive(Default)]
pub(crate) struct FakeVolumes {
    pub(crate) objects: Mutex<BTreeMap<String, Value>>,
    pub(crate) calls: Mutex<Vec<String>>,
    pub(crate) status_writes: Mutex<Vec<(String, Option<NodeStatus>)>>,
    /// The next this many `patch` calls fail with a conflict.
    pub(crate) conflicts: Mutex<u32>,
    pub(crate) version: AtomicU32,
    pub(crate) fail: AtomicBool,
    /// `list` fails while set; the number of failed calls is in `list_failures`.
    pub(crate) fail_list: AtomicBool,
    pub(crate) list_failures: AtomicU32,
    /// `get` of these names fails.
    pub(crate) failing: Mutex<BTreeSet<String>>,
    /// An entry of `status.nodes` that another field manager co-owns: the
    /// empty server-side apply of this node does not remove it.
    pub(crate) co_owned: AtomicBool,
    /// Every JSON patch of the status subresource fails.
    pub(crate) fail_status_patch: AtomicBool,
    /// Runs once at the start of the next `list`.
    pub(crate) on_list: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    pub(crate) events: Mutex<Option<mpsc::UnboundedReceiver<String>>>,
    /// Names `delete` was called with, in order.
    pub(crate) deleted: Mutex<Vec<String>>,
    /// `get`, `list` and `create` give the other tasks a turn first, so that
    /// calls of concurrent handlers interleave.
    pub(crate) yields: AtomicBool,
    /// An object listed here is left out of the next this-many `list` calls,
    /// as a cache that has not seen it yet would.
    /// Only the cached `list` is affected: `list_fresh` always returns the true state.
    pub(crate) hidden: Mutex<BTreeMap<String, u32>>,
    /// `list_fresh` fails while set.
    pub(crate) fail_fresh: AtomicBool,
    /// Number of `list_fresh` calls.
    pub(crate) fresh_lists: AtomicU32,
    /// The next `create` fails with this error.
    pub(crate) fail_create: Mutex<Option<ApiError>>,
    /// Runs once in the next `create`, before the object is stored.
    pub(crate) on_create: Mutex<Option<Hook>>,
    /// Runs once in the next `create`, right after the object is stored.
    pub(crate) after_create: Mutex<Option<Hook>>,
    /// Notified after every stored `create`.
    pub(crate) created: tokio::sync::Notify,
    /// `creationTimestamp` the next `create`s give (RFC 3339); default 2026-01-01T00:00:00Z.
    pub(crate) created_at: Mutex<Option<String>>,
    /// `(name, uid)` of every `delete_exact` call, whether it deleted or not.
    pub(crate) exact_deletes: Mutex<Vec<(String, String)>>,
    /// The next `delete_exact` fails with this error.
    pub(crate) fail_exact: Mutex<Option<ApiError>>,
}

/// A one-shot action of the fake on its own state.
pub(crate) type Hook = Box<dyn FnOnce(&FakeVolumes) + Send>;

/// `creationTimestamp` of an object created by the fake unless `created_at` says otherwise.
pub(crate) const T0: &str = "2026-01-01T00:00:00Z";

pub(crate) fn time(rfc3339: &str) -> k8s_openapi::apimachinery::pkg::apis::meta::v1::Time {
    serde_json::from_value(json!(rfc3339)).unwrap()
}

pub(crate) fn typed(value: &Value) -> ReplicatedVolume {
    serde_json::from_value(value.clone()).unwrap()
}

/// Applies `test`, `replace` and `remove` operations to `document`.
pub(crate) fn apply_ops(document: &mut Value, ops: &[Value]) -> Result<(), String> {
    for op in ops {
        let path = op["path"].as_str().ok_or("no path")?;
        match op["op"].as_str().ok_or("no op")? {
            "test" => {
                if document.pointer(path) != Some(&op["value"]) {
                    return Err(format!("test of {path} failed"));
                }
            }
            "replace" => {
                *document.pointer_mut(path).ok_or(format!("no {path}"))? = op["value"].clone();
            }
            "remove" => {
                let (parent, last) = path.rsplit_once('/').ok_or("bad path")?;
                match document.pointer_mut(parent).ok_or(format!("no {parent}"))? {
                    Value::Array(items) => {
                        let index: usize = last.parse().map_err(|_| "bad index")?;
                        if index >= items.len() {
                            return Err(format!("no {path}"));
                        }
                        items.remove(index);
                    }
                    Value::Object(map) => {
                        map.remove(last).ok_or(format!("no {path}"))?;
                    }
                    _ => return Err(format!("cannot remove {path}")),
                }
            }
            other => return Err(format!("unsupported op {other}")),
        }
    }
    Ok(())
}

impl FakeVolumes {
    async fn turn(&self) {
        if self.yields.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    }

    pub(crate) fn log(&self, line: String) {
        self.calls.lock().unwrap().push(line);
    }

    pub(crate) fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    pub(crate) fn next_version(&self) -> String {
        self.version.fetch_add(1, Ordering::SeqCst).to_string()
    }

    /// Stores the object; one without a uid gets a new one, as the API server would give.
    pub(crate) fn put(&self, mut volume: ReplicatedVolume) {
        volume.metadata.resource_version = Some(self.next_version());
        if volume.metadata.uid.is_none() {
            volume.metadata.uid = Some(format!("uid-{}", self.next_version()));
        }
        let name = volume.metadata.name.clone().unwrap();
        self.objects
            .lock()
            .unwrap()
            .insert(name, serde_json::to_value(&volume).unwrap());
    }

    /// Stores the JSON as it is.
    pub(crate) fn put_raw(&self, name: &str, value: Value) {
        self.objects.lock().unwrap().insert(name.to_string(), value);
    }

    pub(crate) fn raw(&self, name: &str) -> Option<Value> {
        self.objects.lock().unwrap().get(name).cloned()
    }

    pub(crate) fn object(&self, name: &str) -> Option<ReplicatedVolume> {
        self.raw(name).map(|value| typed(&value))
    }

    pub(crate) fn finalizers(&self, name: &str) -> Vec<String> {
        self.object(name)
            .and_then(|o| o.metadata.finalizers)
            .unwrap_or_default()
    }

    pub(crate) fn status_writes(&self) -> Vec<(String, Option<NodeStatus>)> {
        self.status_writes.lock().unwrap().clone()
    }

    pub(crate) fn entry_of(&self, name: &str, node: &str) -> Option<NodeStatus> {
        self.object(name)?.status?.nodes.into_iter().find(|n| n.name == node)
    }

    /// Writes an entry the way `apply_node_status` does, without a call being logged.
    pub(crate) fn set_entry(&self, name: &str, entry: NodeStatus) {
        let mut objects = self.objects.lock().unwrap();
        let object = objects.get_mut(name).unwrap();
        let mut nodes: Vec<NodeStatus> = serde_json::from_value(object["status"]["nodes"].clone()).unwrap_or_default();
        nodes.retain(|n| n.name != entry.name);
        nodes.push(entry);
        object["status"] = json!({ "nodes": nodes });
    }

    pub(crate) fn events(&self) -> mpsc::UnboundedSender<String> {
        let (tx, rx) = mpsc::unbounded_channel();
        *self.events.lock().unwrap() = Some(rx);
        tx
    }
}

#[async_trait]
impl Objects for FakeVolumes {
    async fn read(&self, name: &str) -> Result<Option<ReplicatedVolume>, ApiError> {
        Ok(self.object(name))
    }

    async fn patch(&self, name: &str, ops: Vec<Value>) -> Result<bool, ApiError> {
        self.patch_on(name, ops, false)
    }

    async fn patch_status(&self, name: &str, ops: Vec<Value>) -> Result<bool, ApiError> {
        if self.fail_status_patch.load(Ordering::SeqCst) {
            self.log(format!("patch-status {name} failed"));
            return Err(ApiError::Other("the status patch is refused".into()));
        }
        self.patch_on(name, ops, true)
    }
}

impl FakeVolumes {
    /// A JSON patch of the object or, with `status`, of its status subresource.
    pub(crate) fn patch_on(&self, name: &str, ops: Vec<Value>, status: bool) -> Result<bool, ApiError> {
        let verb = if status { "patch-status" } else { "patch" };
        let injected = {
            let mut left = self.conflicts.lock().unwrap();
            let injected = *left > 0;
            *left = left.saturating_sub(1);
            injected
        };
        if injected {
            self.log(format!("{verb} {name} conflict"));
            return Err(ApiError::Conflict("the object has been modified".into()));
        }
        let mut objects = self.objects.lock().unwrap();
        let Some(stored) = objects.get(name) else {
            return Ok(false);
        };
        let mut next = stored.clone();
        if let Err(why) = apply_ops(&mut next, &ops) {
            drop(objects);
            self.log(format!("{verb} {name} rejected"));
            return Err(ApiError::Rejected(why));
        }
        next["metadata"]["resourceVersion"] = json!(self.next_version());
        let empty = next["metadata"]["finalizers"]
            .as_array()
            .is_some_and(|finalizers| finalizers.is_empty());
        if empty {
            next["metadata"].as_object_mut().unwrap().remove("finalizers");
        }
        let gone = next["metadata"].get("deletionTimestamp").is_some() && next["metadata"].get("finalizers").is_none();
        if gone {
            objects.remove(name);
        } else {
            objects.insert(name.to_string(), next);
        }
        drop(objects);
        self.log(format!("{verb} {name}"));
        Ok(true)
    }
}

#[async_trait]
impl Volumes for FakeVolumes {
    async fn get(&self, name: &str) -> Result<Option<ReplicatedVolume>, ApiError> {
        self.turn().await;
        if self.fail.load(Ordering::SeqCst) || self.failing.lock().unwrap().contains(name) {
            self.log(format!("get {name} failed"));
            return Err(ApiError::Other("the API server is down".into()));
        }
        self.log(format!("get {name}"));
        Ok(self.object(name))
    }

    async fn list(&self) -> Result<Vec<ReplicatedVolume>, ApiError> {
        self.turn().await;
        if self.fail_list.load(Ordering::SeqCst) {
            self.list_failures.fetch_add(1, Ordering::SeqCst);
            return Err(ApiError::Other("the API server is down".into()));
        }
        let hook = self.on_list.lock().unwrap().take();
        if let Some(hook) = hook {
            hook();
        }
        let mut hidden = self.hidden.lock().unwrap();
        for left in hidden.values_mut() {
            *left = left.saturating_sub(1);
        }
        let visible = self
            .objects
            .lock()
            .unwrap()
            .iter()
            .filter(|(name, _)| hidden.get(*name).is_none_or(|left| *left == 0))
            .map(|(_, value)| typed(value))
            .collect();
        Ok(visible)
    }

    async fn list_fresh(&self) -> Result<Vec<ReplicatedVolume>, ApiError> {
        self.turn().await;
        self.fresh_lists.fetch_add(1, Ordering::SeqCst);
        if self.fail_fresh.load(Ordering::SeqCst) {
            return Err(ApiError::Other("the API server is down".into()));
        }
        Ok(self.objects.lock().unwrap().values().map(typed).collect())
    }

    async fn create(&self, volume: &ReplicatedVolume) -> Result<Created, ApiError> {
        self.turn().await;
        let failure = self.fail_create.lock().unwrap().take();
        if let Some(failure) = failure {
            return Err(failure);
        }
        let hook = self.on_create.lock().unwrap().take();
        if let Some(hook) = hook {
            hook(self);
        }
        let name = volume.metadata.name.clone().unwrap();
        if self.object(&name).is_some() {
            return Ok(Created::Exists);
        }
        let mut volume = volume.clone();
        let stamp = self
            .created_at
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| T0.to_string());
        volume.metadata.creation_timestamp = Some(time(&stamp));
        self.put(volume);
        let uid = self.object(&name).and_then(|o| o.metadata.uid).unwrap();
        let hook = self.after_create.lock().unwrap().take();
        if let Some(hook) = hook {
            hook(self);
        }
        self.created.notify_one();
        Ok(Created::New(uid))
    }

    async fn delete_exact(&self, name: &str, uid: &str) -> Result<(), ApiError> {
        self.exact_deletes
            .lock()
            .unwrap()
            .push((name.to_string(), uid.to_string()));
        let failure = self.fail_exact.lock().unwrap().take();
        if let Some(failure) = failure {
            return Err(failure);
        }
        match self.object(name) {
            None => Ok(()),
            Some(object) if object.metadata.uid.as_deref() == Some(uid) => self.delete(name).await,
            Some(_) => Err(ApiError::Conflict("the uid precondition failed".into())),
        }
    }

    async fn delete(&self, name: &str) -> Result<(), ApiError> {
        self.deleted.lock().unwrap().push(name.to_string());
        let mut objects = self.objects.lock().unwrap();
        let has_finalizers = objects
            .get(name)
            .is_some_and(|o| o["metadata"]["finalizers"].as_array().is_some_and(|f| !f.is_empty()));
        if has_finalizers {
            objects.get_mut(name).unwrap()["metadata"]["deletionTimestamp"] = json!("1970-01-01T00:00:00Z");
        } else {
            objects.remove(name);
        }
        Ok(())
    }

    async fn remove_finalizer(&self, name: &str, uid: &str, finalizer: &str) -> Result<bool, ApiError> {
        patch_retrying(self, name, uid, &|object| remove_finalizer_ops(object, finalizer)).await
    }

    async fn mark_initial_sync_done(&self, name: &str, uid: &str) -> Result<bool, ApiError> {
        patch_retrying(self, name, uid, &initial_sync_done_ops).await
    }

    async fn remove_node_entry(&self, name: &str, uid: &str, node: &str) -> Result<bool, ApiError> {
        patch_status_retrying(self, name, uid, &|object| remove_node_entry_ops(object, node)).await
    }

    async fn apply_node_status(&self, name: &str, node: &str, entry: Option<&NodeStatus>) -> Result<(), ApiError> {
        self.log(format!(
            "status {name} {node} {}",
            if entry.is_some() { "set" } else { "removed" }
        ));
        self.status_writes
            .lock()
            .unwrap()
            .push((node.to_string(), entry.cloned()));
        if self.object(name).is_some() {
            if let Some(entry) = entry {
                self.set_entry(name, entry.clone());
            } else if !self.co_owned.load(Ordering::SeqCst) {
                let mut objects = self.objects.lock().unwrap();
                let object = objects.get_mut(name).unwrap();
                let mut nodes: Vec<NodeStatus> =
                    serde_json::from_value(object["status"]["nodes"].clone()).unwrap_or_default();
                nodes.retain(|n| n.name != node);
                object["status"] = json!({ "nodes": nodes });
            }
        }
        Ok(())
    }

    fn watch(&self) -> BoxStream<'static, Result<String, ApiError>> {
        match self.events.lock().unwrap().take() {
            Some(rx) => stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|name| (Ok(name), rx)) }).boxed(),
            None => stream::pending().boxed(),
        }
    }
}

/// A node map held in memory; `Err` answers every `load` with that error.
pub(crate) struct FakeNodes(pub(crate) Result<ClusterNodes, ApiError>);

#[async_trait]
impl NodeMap for FakeNodes {
    async fn load(&self) -> Result<ClusterNodes, ApiError> {
        self.0.clone()
    }
}

/// Nodes `n1..` with the given capacities; node ids 0.., addresses 192.0.2.1...
pub(crate) fn cluster(caps: &[u64]) -> ClusterNodes {
    let list: Vec<String> = caps
        .iter()
        .enumerate()
        .map(|(i, cap)| {
            format!(
                r#"{{"name":"n{}","address":"192.0.2.{}","nodeId":{},"capacityBytes":{}}}"#,
                i + 1,
                i + 1,
                i,
                cap
            )
        })
        .collect();
    ClusterNodes::from_json(&format!("[{}]", list.join(","))).unwrap()
}
