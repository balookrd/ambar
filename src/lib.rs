//! ambar: replicated volumes for k3s on top of DRBD 9.

pub mod backing;
pub mod drbd;
pub mod exec;
pub mod model;
pub mod module_loader;
pub mod paths;

#[cfg(test)]
mod tests;
