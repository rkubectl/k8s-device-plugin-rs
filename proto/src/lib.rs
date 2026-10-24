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

    #[cfg(feature = "dra-health")]
    pub mod health {

        pub mod v1alpha1 {

            pub use dra_resource_health_client::DraResourceHealthClient;
            pub use dra_resource_health_server::DraResourceHealth;
            pub use dra_resource_health_server::DraResourceHealthServer;
            pub use dra_resource_health_server::SERVICE_NAME;

            tonic::include_proto!("v1alpha1");
        }
    }
}

impl v1beta1::RegisterRequest {
    /// Builds a request for this crate's supported API version, with no options.
    pub fn new(endpoint: &str, resource_name: &str) -> Self {
        Self {
            version: v1beta1::VERSION.to_string(),
            endpoint: endpoint.to_string(),
            resource_name: resource_name.to_string(),
            options: None,
        }
    }

    /// Sets pre-start support, preserving the other registration options.
    #[must_use]
    pub fn pre_start_required(mut self, yes: bool) -> Self {
        self.options.get_or_insert_default().pre_start_required = yes;
        self
    }

    /// Sets preferred-allocation support, preserving the other options.
    #[must_use]
    pub fn get_preferred_allocation_available(mut self, yes: bool) -> Self {
        self.options
            .get_or_insert_default()
            .get_preferred_allocation_available = yes;
        self
    }

    /// Replaces all registration options.
    #[must_use]
    pub fn options(self, options: v1beta1::DevicePluginOptions) -> Self {
        Self {
            options: Some(options),
            ..self
        }
    }
}
