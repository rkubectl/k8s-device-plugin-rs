# Build from the repository root:
# container build -f dra/examples/accelerator-pairs/Dockerfile \
#   -t accelerator-pair-dra-example .

ARG RUST_VERSION=1.98

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
        -p k8s-device-plugin-dra \
        --example accelerator_pair_driver \
        --example liveness_probe \
    && cp "target/$(cat /rust_target)/release/examples/accelerator_pair_driver" \
        /out/accelerator-pair-dra-driver \
    && cp "target/$(cat /rust_target)/release/examples/liveness_probe" \
        /out/k8s-device-plugin-dra-liveness

FROM gcr.io/distroless/static-debian12 AS runtime
COPY --from=builder /out/accelerator-pair-dra-driver /usr/local/bin/accelerator-pair-dra-driver
COPY --from=builder /out/k8s-device-plugin-dra-liveness /usr/local/bin/k8s-device-plugin-dra-liveness
ENTRYPOINT ["/usr/local/bin/accelerator-pair-dra-driver"]
