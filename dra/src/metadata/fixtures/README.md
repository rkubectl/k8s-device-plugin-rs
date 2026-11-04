These compact version-1 journals were captured from the JSON implementation at
`197a277`, before the typed model migration in `k8s-device-plugin-rs-420`.
The host plugin directory was fixed to `/node/plugin`. The template fixture
adds the `pod-local` annotation; both include two isolated requests, all four
scalar attribute kinds and an empty attribute map.

The embedded document/spec strings include their final newline; the journal
itself has none. Keep these files byte-for-byte: they verify serialization,
retry inode preservation, and cleanup of artifacts produced by older releases.
