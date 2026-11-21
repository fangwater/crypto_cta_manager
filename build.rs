fn main() {
    println!("cargo:rerun-if-changed=src/web/timeline.proto");
    prost_build::compile_protos(&["src/web/timeline.proto"], &["src/web"])
        .expect("compile the NAV browser Protobuf contract (requires protoc)");
    println!("cargo:rerun-if-changed=src/position_archive/storage.proto");
    prost_build::compile_protos(
        &["src/position_archive/storage.proto"],
        &["src/position_archive"],
    )
    .expect("compile the Manager position archive storage contract (requires protoc)");
    println!("cargo:rerun-if-changed=src/native_futures/position_target.proto");
    tonic_prost_build::configure()
        .compile_protos(
            &["src/native_futures/position_target.proto"],
            &["src/native_futures"],
        )
        .expect("compile the native Exec target contract (requires protoc)");
}
