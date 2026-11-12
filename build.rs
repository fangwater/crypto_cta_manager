fn main() {
    println!("cargo:rerun-if-changed=src/web/timeline.proto");
    prost_build::compile_protos(&["src/web/timeline.proto"], &["src/web"])
        .expect("compile the NAV browser Protobuf contract (requires protoc)");
}
