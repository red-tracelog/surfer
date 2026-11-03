fn main() {
    let protoc = protoc_bin_vendored::protoc_bin_path().expect("failed to find bundled protoc");
    unsafe {
        std::env::set_var("PROTOC", protoc);
    }

    let generated_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("generated");
    std::fs::create_dir_all(&generated_dir).expect("failed to create generated directory");

    println!("cargo:rerun-if-changed=proto/simulator_protocol.proto");
    println!("cargo:rerun-if-changed=build.rs");

    prost_build::Config::new()
        .type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]")
        .out_dir(generated_dir)
        .compile_protos(&["proto/simulator_protocol.proto"], &["proto"])
        .expect("failed to compile protobuf schema");
}
