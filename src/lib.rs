//! ambar: replicated volumes for k3s on top of DRBD 9.

pub mod agent;
pub mod backing;
pub mod cli;
pub mod config;
pub mod crd;
pub mod csi;
pub mod drbd;
pub mod exec;
pub mod membership;
pub mod model;
pub mod module_loader;
pub mod mount;
pub mod paths;
pub mod placement;
pub mod reconcile;
pub mod replicas;

#[cfg(test)]
mod tests;
