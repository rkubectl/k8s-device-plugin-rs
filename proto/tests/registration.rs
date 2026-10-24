use prost::Message;

use k8s_device_plugin_proto::v1beta1;

#[test]
fn constructor_preserves_the_default_registration_wire_request() {
    let request = v1beta1::RegisterRequest::new("plugin.sock", "example.com/widget");
    let expected = v1beta1::RegisterRequest {
        version: "v1beta1".into(),
        endpoint: "plugin.sock".into(),
        resource_name: "example.com/widget".into(),
        options: None,
    };
    assert_eq!(request.encode_to_vec(), expected.encode_to_vec());
}

#[test]
fn option_builders_merge_flags_and_allow_explicit_false() {
    let request = v1beta1::RegisterRequest::new("plugin.sock", "example.com/widget")
        .pre_start_required(true)
        .get_preferred_allocation_available(true);
    let expected = v1beta1::DevicePluginOptions {
        pre_start_required: true,
        get_preferred_allocation_available: true,
    };
    assert_eq!(request.options, Some(expected));
    let request = request.pre_start_required(false);
    assert_eq!(
        request.options,
        Some(v1beta1::DevicePluginOptions {
            pre_start_required: false,
            get_preferred_allocation_available: true,
        })
    );
}

#[test]
fn explicit_options_replace_flags_and_later_setters_preserve_them() {
    let options = v1beta1::DevicePluginOptions {
        pre_start_required: false,
        get_preferred_allocation_available: true,
    };
    let request = v1beta1::RegisterRequest::new("plugin.sock", "example.com/widget")
        .pre_start_required(true)
        .options(options);
    assert_eq!(request.options, Some(options));
    assert_eq!(
        request.get_preferred_allocation_available(false).options,
        Some(v1beta1::DevicePluginOptions {
            pre_start_required: false,
            get_preferred_allocation_available: false,
        })
    );
}
