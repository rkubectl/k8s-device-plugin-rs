fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::configure()
        .compile_protos(&["kubelet/pkg/apis/deviceplugin/v1beta1/api.proto"], &[])?;

    if std::env::var("CARGO_FEATURE_DRA").is_ok() {
        tonic_prost_build::configure().compile_protos(
            &[
                "kubelet/pkg/apis/dra/v1/api.proto",
                "kubelet/pkg/apis/pluginregistration/v1/api.proto",
            ],
            &[],
        )?;
    }

    if std::env::var("CARGO_FEATURE_DRA_HEALTH").is_ok() {
        tonic_prost_build::configure()
            .compile_protos(&["kubelet/pkg/apis/dra-health/v1alpha1/api.proto"], &[])?;
    }

    Ok(())
}
