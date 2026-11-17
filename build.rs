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
}
