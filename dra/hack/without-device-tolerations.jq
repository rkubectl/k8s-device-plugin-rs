# kubectl create -o json can emit a stream of objects or a List.
{
  apiVersion: "v1",
  kind: "List",
  items: [.[] | if .kind == "List" then .items[] else . end] |
    map(if .kind == "ResourceClaim" then del(.spec.devices.requests[].exactly.tolerations) else . end)
}
