#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUTPUT_DIR="$ROOT_DIR/dist/sbom"
VERSION="${VERSION:-dev}"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --output)
            OUTPUT_DIR="$2"
            shift 2
            ;;
        --version)
            VERSION="$2"
            shift 2
            ;;
        *)
            echo "unknown argument: $1" >&2
            exit 2
            ;;
    esac
done

if ! command -v cargo-cyclonedx >/dev/null 2>&1; then
    echo "cargo-cyclonedx is required; install with: cargo install cargo-cyclonedx --version 0.5.7 --locked" >&2
    exit 1
fi

mkdir -p "$OUTPUT_DIR"

cleanup_generated() {
    find "$ROOT_DIR" -path "$ROOT_DIR/target" -prune -o -name '*_bin_*-unknown-linux-musl.cdx.json' -type f -delete
}
trap cleanup_generated EXIT
cleanup_generated

generate_for_target() {
    local target="$1"
    local arch="$2"

    (
        cd "$ROOT_DIR"
        cargo cyclonedx \
            --format json \
            --describe binaries \
            --target "$target" \
            --target-in-filename
    )

    move_bom "operator/fluidbg-operator_bin_${target}.cdx.json" \
        "fbg-operator-${VERSION}-linux-${arch}.cyclonedx.json"
    move_bom "plugins/http/fluidbg-http_bin_${target}.cdx.json" \
        "fbg-plugin-http-${VERSION}-linux-${arch}.cyclonedx.json"
    move_bom "plugins/rabbitmq/fluidbg-rabbitmq_bin_${target}.cdx.json" \
        "fbg-plugin-rabbitmq-${VERSION}-linux-${arch}.cyclonedx.json"
    move_bom "plugins/azure_servicebus/fluidbg-azure-servicebus_bin_${target}.cdx.json" \
        "fbg-plugin-azure-servicebus-${VERSION}-linux-${arch}.cyclonedx.json"
}

move_bom() {
    local source="$1"
    local dest="$2"
    if [ ! -f "$ROOT_DIR/$source" ]; then
        echo "expected SBOM was not generated: $source" >&2
        exit 1
    fi
    mv "$ROOT_DIR/$source" "$OUTPUT_DIR/$dest"
}

generate_for_target x86_64-unknown-linux-musl amd64
generate_for_target aarch64-unknown-linux-musl arm64

find "$OUTPUT_DIR" -type f -name '*.cyclonedx.json' -print | sort
