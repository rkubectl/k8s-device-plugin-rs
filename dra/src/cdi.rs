//! Private CDI 0.8 projection shared with the validation examples. This is not
//! a general CDI API: only environment markers and metadata mounts are modeled.
use serde::Serialize;

// Declaration order is canonical JSON key order. Immutable retry artifacts
// must remain byte-identical to the original serde_json::Value encoding.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Spec {
    pub cdi_version: &'static str,
    pub devices: Vec<Device>,
    pub kind: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Device {
    pub container_edits: ContainerEdits,
    pub name: String,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct ContainerEdits {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<Mount>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Mount {
    pub container_path: String,
    pub host_path: String,
    pub options: Vec<&'static str>,
}

#[cfg(test)]
mod tests {
    use serde_json as json;

    use super::*;

    #[test]
    fn environment_specs_preserve_pretty_wire_bytes_and_omit_mounts() {
        let spec = Spec {
            cdi_version: "0.8.0",
            devices: vec![Device {
                container_edits: ContainerEdits {
                    env: vec!["DEVICE=widget-0".into(), "MEMBERS=left,right".into()],
                    ..Default::default()
                },
                name: "widget-0".into(),
            }],
            kind: "example.com/widget".into(),
        };
        // The examples historically serialized a Value with sorted object keys.
        let legacy = json::json!({
            "cdiVersion": "0.8.0",
            "kind": "example.com/widget",
            "devices": [{
                "name": "widget-0",
                "containerEdits": {"env": ["DEVICE=widget-0", "MEMBERS=left,right"]}
            }]
        });
        assert_eq!(
            json::to_vec_pretty(&spec).unwrap(),
            json::to_vec_pretty(&legacy).unwrap()
        );
    }
}
