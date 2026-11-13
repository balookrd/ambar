//! Node agent: brings the node in line with `ReplicatedVolume` objects.
//! `decide` and `status` are pure decisions; `local` reads the node, `kube`
//! makes the Kubernetes calls and `run` is the reconcile loop that ties them
//! to `reconcile::Reconciler`.

pub mod decide;
pub mod kube;
pub mod local;
pub mod run;
pub mod status;
