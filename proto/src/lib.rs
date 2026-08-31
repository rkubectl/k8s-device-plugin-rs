pub mod v1beta1 {

    pub const HEALTHY: &str = "Healthy";
    pub const UNHEALTHY: &str = "Unhealthy";
    pub const VERSION: &str = "v1beta1";

    #[cfg(not(windows))]
    pub const DEVICE_PLUGIN_PATH: &str = "/var/lib/kubelet/device-plugins/";
    #[cfg(windows)]
    pub const DEVICE_PLUGIN_PATH: &str = "\\var\\lib\\kubelet\\device-plugins\\";

    pub const KUBELET_SOCKET: &str = "kubelet.sock";

    // const KUBELET_PRE_START_CONTAINER_RPC_TIMEOUT_IN_SECS: u64 = 30;

    pub use device_plugin_client::DevicePluginClient;
    pub use device_plugin_server::DevicePlugin;
    pub use device_plugin_server::DevicePluginServer;
    pub use device_plugin_server::SERVICE_NAME;
    pub use registration_client::RegistrationClient;
    pub use registration_server::Registration;
    pub use registration_server::RegistrationServer;

    tonic::include_proto!("v1beta1");
}

#[cfg(feature = "dra")]
pub mod dra {

    /// Registration identifier for the stable kubelet DRA gRPC service.
    ///
    /// Kubelet compares this value with `PluginInfo.supported_versions` when
    /// registering a `DRAPlugin`; it is a service identifier, not merely an
    /// API version.
    pub const DRA_PLUGIN_SERVICE: &str = "v1.DRAPlugin";

    #[cfg(not(windows))]
    pub const KUBELET_PLUGINS_PATH: &str = "/var/lib/kubelet/plugins/";
    #[cfg(windows)]
    pub const KUBELET_PLUGINS_PATH: &str = "\\var\\lib\\kubelet\\plugins\\";

    #[cfg(not(windows))]
    pub const KUBELET_PLUGINS_REGISTRY_PATH: &str = "/var/lib/kubelet/plugins_registry/";
    #[cfg(windows)]
    pub const KUBELET_PLUGINS_REGISTRY_PATH: &str = "\\var\\lib\\kubelet\\plugins_registry\\";

    pub mod v1 {

        pub use dra_plugin_client::DraPluginClient;
        pub use dra_plugin_server::DraPlugin;
        pub use dra_plugin_server::DraPluginServer;
        pub use dra_plugin_server::SERVICE_NAME;

        tonic::include_proto!("k8s.io.kubelet.pkg.apis.dra.v1");
    }

    pub mod registration {

        pub use registration_client::RegistrationClient;
        pub use registration_server::Registration;
        pub use registration_server::RegistrationServer;
        pub use registration_server::SERVICE_NAME;

        tonic::include_proto!("pluginregistration");
    }
}
