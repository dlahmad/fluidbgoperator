#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="${DIST_DIR:-$ROOT_DIR/dist}"
TARGET_ARCH="${TARGET_ARCH:-}"
TARGET_TRIPLE="${TARGET_TRIPLE:-}"
LOCAL=false

while [ "$#" -gt 0 ]; do
    case "$1" in
        --local)
            LOCAL=true
            shift
            ;;
        --arch)
            TARGET_ARCH="$2"
            shift 2
            ;;
        --target)
            TARGET_TRIPLE="$2"
            shift 2
            ;;
        *)
            echo "unknown argument: $1" >&2
            exit 2
            ;;
    esac
done

if [ -z "$TARGET_ARCH" ]; then
    TARGET_ARCH="$(uname -m)"
fi

case "$TARGET_ARCH" in
    x86_64|amd64)
        TARGET_ARCH="amd64"
        TARGET_TRIPLE="${TARGET_TRIPLE:-x86_64-unknown-linux-musl}"
        ;;
    arm64|aarch64)
        TARGET_ARCH="arm64"
        TARGET_TRIPLE="${TARGET_TRIPLE:-aarch64-unknown-linux-musl}"
        ;;
    *)
        echo "unsupported target architecture: $TARGET_ARCH" >&2
        exit 2
        ;;
esac

mkdir -p "$DIST_DIR"

if [ "$LOCAL" = true ]; then
    cargo build --release --locked --target "$TARGET_TRIPLE" --bin fluidbg-operator
    cargo build --release --locked --target "$TARGET_TRIPLE" -p fluidbg-http
    cargo build --release --locked --target "$TARGET_TRIPLE" -p fluidbg-rabbitmq
    TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT_DIR/target}"
    cp "$TARGET_DIR/$TARGET_TRIPLE/release/fluidbg-operator" "$DIST_DIR/fluidbg-operator"
    cp "$TARGET_DIR/$TARGET_TRIPLE/release/fluidbg-http" "$DIST_DIR/fluidbg-http"
    cp "$TARGET_DIR/$TARGET_TRIPLE/release/fluidbg-rabbitmq" "$DIST_DIR/fluidbg-rabbitmq"
else
    mkdir -p "$ROOT_DIR/.docker-target" "$ROOT_DIR/.docker-cargo-home/registry" "$ROOT_DIR/.docker-cargo-home/git"
    docker run --rm --platform "linux/$TARGET_ARCH" \
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
        bash -lc "
            set -euo pipefail
            export PATH=/usr/local/cargo/bin:\$PATH
            apt-get update
            apt-get install -y --no-install-recommends musl-tools
            rm -rf /var/lib/apt/lists/*
            rustup target add '$TARGET_TRIPLE'
            cargo build --release --locked --target '$TARGET_TRIPLE' --bin fluidbg-operator
            cargo build --release --locked --target '$TARGET_TRIPLE' -p fluidbg-http
            cargo build --release --locked --target '$TARGET_TRIPLE' -p fluidbg-rabbitmq
            cp '/cargo-target/$TARGET_TRIPLE/release/fluidbg-operator' /work/dist/fluidbg-operator
            cp '/cargo-target/$TARGET_TRIPLE/release/fluidbg-http' /work/dist/fluidbg-http
            cp '/cargo-target/$TARGET_TRIPLE/release/fluidbg-rabbitmq' /work/dist/fluidbg-rabbitmq
        "
fi

printf 'Built %s release binaries in %s\n' "$TARGET_TRIPLE" "$DIST_DIR"
