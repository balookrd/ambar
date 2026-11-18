//! Node agent: brings the node in line with `ReplicatedVolume` objects.
//! `decide` and `status` are pure decisions; `local` reads the node, `kube`
//! makes the Kubernetes calls and `run` is the reconcile loop that ties them
//! to `reconcile::Reconciler`. `nodes` reads the node map ConfigMap for the
//! CSI Controller.

pub mod decide;
pub mod kube;
pub mod local;
pub mod nodes;
pub mod replica_cmd;
pub mod run;
pub mod status;
pub mod taint;
