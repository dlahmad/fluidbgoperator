#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
NS="${NS:-fluidbg-test}"
KIND_CLUSTER="${KIND_CLUSTER:-}"
BUILD_IMAGES="${BUILD_IMAGES:-1}"

target_arch() {
    local arch
    arch="${TARGET_ARCH:-}"
    if [ -z "$arch" ]; then
        arch="$(kubectl get nodes -o jsonpath='{.items[0].status.nodeInfo.architecture}' 2>/dev/null || true)"
    fi
    if [ -z "$arch" ]; then
        arch="amd64"
    fi
    printf '%s\n' "$arch"
}

build_linux_rust_binaries() {
    local arch="$1"
    local platform="linux/$arch"
    mkdir -p \
        "$ROOT_DIR/dist" \
        "$ROOT_DIR/.docker-target" \
        "$ROOT_DIR/.docker-cargo-home/registry" \
        "$ROOT_DIR/.docker-cargo-home/git"
    docker run --rm --platform "$platform" \
        -v "$ROOT_DIR":/work \
        -v "$ROOT_DIR/.docker-target":/cargo-target \
        -v "$ROOT_DIR/.docker-cargo-home":/cargo-home \
        -e CARGO_HOME=/cargo-home \
        -e CARGO_TARGET_DIR=/cargo-target \
        -e CARGO_HTTP_TIMEOUT=600 \
        -e CARGO_NET_RETRY=10 \
        -e CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse \
        -w /work \
        rust:bookworm \
        bash -lc '
            set -euo pipefail
            export PATH=/usr/local/cargo/bin:$PATH
            cargo build --release --bin fluidbg-operator
            cargo build --release -p fluidbg-rabbitmq
            cp /cargo-target/release/fluidbg-operator /work/dist/fluidbg-operator
            cp /cargo-target/release/fluidbg-rabbitmq /work/dist/fluidbg-rabbitmq
        '
}

prefetch_linux_rust_dependencies() {
    local arch="$1"
    local platform="linux/$arch"
    mkdir -p "$ROOT_DIR/.docker-cargo-home/registry" "$ROOT_DIR/.docker-cargo-home/git"
    docker run --rm --platform "$platform" \
        -v "$ROOT_DIR":/work \
        -v "$ROOT_DIR/.docker-cargo-home":/cargo-home \
        -e CARGO_HOME=/cargo-home \
        -e CARGO_HTTP_TIMEOUT=600 \
        -e CARGO_NET_RETRY=10 \
        -e CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse \
        -w /work \
        rust:bookworm \
        bash -lc '
            set -euo pipefail
            export PATH=/usr/local/cargo/bin:$PATH
            cargo fetch --locked
        '
}

cd "$ROOT_DIR"

cargo run -p fluidbg-operator --bin gen-crds

if [ "$BUILD_IMAGES" = "1" ]; then
    IMAGE_ARCH="$(target_arch)"
    prefetch_linux_rust_dependencies "$IMAGE_ARCH"
    build_linux_rust_binaries "$IMAGE_ARCH"
    docker build --platform "linux/$IMAGE_ARCH" -t fluidbg/fbg-operator:dev "$ROOT_DIR"
    docker build --platform "linux/$IMAGE_ARCH" -f "$ROOT_DIR/plugins/rabbitmq/Dockerfile" -t fluidbg/fbg-plugin-rabbitmq:dev "$ROOT_DIR"
    docker build -t fluidbg/blue-app:dev "$ROOT_DIR/e2e/blue-app"
    docker build -t fluidbg/green-app:dev "$ROOT_DIR/e2e/green-app"
    docker build -t fluidbg/test-app:dev "$ROOT_DIR/e2e/test-app"

    if command -v kind >/dev/null 2>&1; then
        if [ -z "$KIND_CLUSTER" ]; then
            KIND_CLUSTERS="$(kind get clusters 2>/dev/null || true)"
            if [ "$(printf '%s\n' "$KIND_CLUSTERS" | sed '/^$/d' | wc -l | tr -d ' ')" = "1" ]; then
                KIND_CLUSTER="$(printf '%s\n' "$KIND_CLUSTERS" | sed '/^$/d')"
            fi
        fi

        if [ -n "$KIND_CLUSTER" ] && kind get clusters | grep -qx "$KIND_CLUSTER"; then
            kind load docker-image fluidbg/fbg-operator:dev --name "$KIND_CLUSTER"
            kind load docker-image fluidbg/fbg-plugin-rabbitmq:dev --name "$KIND_CLUSTER"
            kind load docker-image fluidbg/blue-app:dev --name "$KIND_CLUSTER"
            kind load docker-image fluidbg/green-app:dev --name "$KIND_CLUSTER"
            kind load docker-image fluidbg/test-app:dev --name "$KIND_CLUSTER"
        fi
    fi
fi

kubectl apply --server-side --force-conflicts -f "$ROOT_DIR/crds/blue_green_deployment.yaml"
kubectl apply --server-side --force-conflicts -f "$ROOT_DIR/crds/inception_plugin.yaml"
kubectl apply --server-side --force-conflicts -f "$ROOT_DIR/crds/state_store.yaml"

kubectl delete bluegreendeployment order-processor-bg -n "$NS" --ignore-not-found
kubectl delete bluegreendeployment order-processor-bootstrap -n "$NS" --ignore-not-found
kubectl delete bluegreendeployment order-processor-upgrade -n "$NS" --ignore-not-found
kubectl delete bluegreendeployment order-processor-failing-upgrade -n "$NS" --ignore-not-found
kubectl delete statestore memory-store -n "$NS" --ignore-not-found
kubectl delete inceptionplugin rabbitmq -n "$NS" --ignore-not-found
kubectl delete job order-processor-trigger-tests -n "$NS" --ignore-not-found
kubectl delete job order-processor-trigger-failing-tests -n "$NS" --ignore-not-found
kubectl delete deployment -n "$NS" -l fluidbg.io/name=order-processor --ignore-not-found
kubectl delete deployment test-container -n "$NS" --ignore-not-found
kubectl delete service test-container -n "$NS" --ignore-not-found
kubectl delete deployment,service,configmap -n "$NS" -l fluidbg.io/inception-point --ignore-not-found
kubectl delete deployment rabbitmq -n fluidbg-system --ignore-not-found
kubectl delete service rabbitmq -n fluidbg-system --ignore-not-found
kubectl delete secret rabbitmq-secret -n fluidbg-system --ignore-not-found
kubectl delete deployment httpbin -n fluidbg-system --ignore-not-found
kubectl delete service httpbin -n fluidbg-system --ignore-not-found
