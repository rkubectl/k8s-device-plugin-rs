# syntax=docker/dockerfile:1
#
# Build from the repository root so the workspace and proto submodule are
# available to Cargo:
#
#   git submodule update --init
#   docker build -f dra/Dockerfile -t k8s-device-plugin-dra-example .

ARG RUST_VERSION=1.90

FROM rust:${RUST_VERSION}-slim AS builder
ARG TARGETARCH
RUN apt-get update \
    && apt-get install -y --no-install-recommends protobuf-compiler musl-tools \
    && rm -rf /var/lib/apt/lists/*
RUN case "${TARGETARCH}" in \
        amd64) echo x86_64-unknown-linux-musl > /rust_target ;; \
        arm64) echo aarch64-unknown-linux-musl > /rust_target ;; \
        *) echo "unsupported TARGETARCH: ${TARGETARCH}" >&2; exit 1 ;; \
    esac
RUN rustup target add "$(cat /rust_target)"
WORKDIR /src
COPY . .
RUN mkdir /out \
    && cargo build --release --locked --target "$(cat /rust_target)" \
        -p k8s-device-plugin-dra --example minimal_driver \
    && cp "target/$(cat /rust_target)/release/examples/minimal_driver" \
        /out/k8s-device-plugin-dra-example

# The kubelet plugin directories mounted by dra/k8s are normally root-owned.
# If a target cluster permits a non-root driver, use a non-root runtime image
# and adjust the manifest's security context accordingly.
FROM gcr.io/distroless/static-debian12 AS runtime
COPY --from=builder /out/k8s-device-plugin-dra-example /usr/local/bin/k8s-device-plugin-dra-example
ENTRYPOINT ["/usr/local/bin/k8s-device-plugin-dra-example"]
