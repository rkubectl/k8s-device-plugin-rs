use k8s_device_plugin_core::ContainerAllocation;
use k8s_device_plugin_core::Device;
use k8s_device_plugin_core::DevicePath;
use k8s_device_plugin_core::HostMount;

#[test]
fn validation_checks_protocol_fields_without_requiring_existing_files() {
    let device = Device::rdwr("widget-0".parse().unwrap(), "/nonexistent/widget");
    device.validate().unwrap();
    assert!(k8s_device_plugin_core::DeviceId::try_from("").is_err());
    assert!(
        Device::rdwr("widget-0".parse().unwrap(), "relative")
            .validate()
            .is_err()
    );
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

#[test]
fn rejects_nul_in_each_allocation_path() {
    for host in [true, false] {
        let mut device = DevicePath::rdwr("/dev/ok");
        if host {
            device.host_path = "/dev/a\0b".into();
        } else {
            device.container_path = "/dev/a\0b".into();
        }
        assert!(matches!(
            device.validate(),
            Err(k8s_device_plugin_core::ValidationError::NulByte(_))
        ));
        assert!(
            ContainerAllocation {
                device_paths: vec![device],
                ..default()
            }
            .validate()
            .is_err()
        );
        let mount = HostMount {
            host_path: if host { "/mnt/a\0b" } else { "/mnt/ok" }.into(),
            container_path: if host { "/mnt/ok" } else { "/mnt/a\0b" }.into(),
            read_only: false,
        };
        assert!(matches!(
            mount.validate(),
            Err(k8s_device_plugin_core::ValidationError::NulByte(_))
        ));
        assert!(
            ContainerAllocation {
                mounts: vec![mount],
                ..default()
            }
            .validate()
            .is_err()
        );
    }
}
