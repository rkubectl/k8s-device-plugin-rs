fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=build.rs");

    let mut protos = vec!["kubelet/pkg/apis/deviceplugin/v1beta1/api.proto"];

    if std::env::var("CARGO_FEATURE_DRA").is_ok() {
        protos.extend([
            "kubelet/pkg/apis/dra/v1/api.proto",
            "kubelet/pkg/apis/pluginregistration/v1/api.proto",
        ]);
    }

    if std::env::var("CARGO_FEATURE_DRA_HEALTH").is_ok() {
        protos.push("kubelet/pkg/apis/dra-health/v1alpha1/api.proto");
    }

    // Keep Tonic's Rust generator; protox supplies descriptors without protoc.
    // https://docs.rs/tonic-prost-build/0.14.6/tonic_prost_build/struct.Builder.html#method.compile_fds
    let mut compiler = protox::Compiler::new(["."])?;
    compiler
        .include_imports(true)
        .include_source_info(true)
        .open_files(protos)?;

    for file in compiler.files() {
        if let Some(path) = file.path() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }

    tonic_prost_build::configure().compile_fds(compiler.file_descriptor_set())?;

    Ok(())
}
