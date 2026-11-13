//! Generates the CSI v1 message types and gRPC services from `proto/csi.proto`.
//!
//! The proto is parsed by `protox` (a Rust implementation of the protobuf
//! compiler), so no system `protoc` is needed on a developer machine, in CI
//! or in the cross build for `aarch64-unknown-linux-musl`.

fn main() {
    println!("cargo:rerun-if-changed=proto/csi.proto");
    println!("cargo:rerun-if-changed=build.rs");
    let descriptors = protox::compile(["csi.proto"], ["proto"]).expect("proto/csi.proto does not compile");
    tonic_prost_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_fds(descriptors)
        .expect("cannot generate the CSI code");
}
