use k8s_device_plugin_core::ContainerAllocation;
use k8s_device_plugin_core::Device;
use k8s_device_plugin_core::DevicePath;
use k8s_device_plugin_core::HostMount;

#[test]
fn validation_checks_protocol_fields_without_requiring_existing_files() {
    let device = Device::rdwr("widget-0", "/nonexistent/widget");
    device.validate().unwrap();
    assert!(Device::new("").validate().is_err());
    assert!(Device::rdwr("widget-0", "relative").validate().is_err());
    let mut path = DevicePath::rdwr("/dev/widget");
    path.container_path = "relative".into();
    assert!(path.validate().is_err());
    let allocation = ContainerAllocation {
        device_paths: device.paths,
        mounts: vec![HostMount {
            host_path: "/nonexistent/config".into(),
            container_path: "relative".into(),
            read_only: true,
        }],
        ..default()
    };
    assert!(allocation.validate().is_err());
}

fn default<T: Default>() -> T {
    T::default()
}
