#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REGISTRY="fluidbg"
TAGS=()
PUSH=false
PLATFORM="${PLATFORM:-}"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --registry)
            REGISTRY="$2"
            shift 2
            ;;
        --tag)
            TAGS+=("$2")
            shift 2
            ;;
        --push)
            PUSH=true
            shift
            ;;
        --platform)
            PLATFORM="$2"
            shift 2
            ;;
        *)
            echo "unknown argument: $1" >&2
            exit 2
            ;;
    esac
done

if [ "${#TAGS[@]}" -eq 0 ]; then
    TAGS=("dev")
fi

build_image() {
    local name="$1"
    local dockerfile="$2"
    shift 2

    local tag_args=()
    local tag
    for tag in "${TAGS[@]}"; do
        tag_args+=("-t" "$REGISTRY/$name:$tag")
    done

    if [ -n "$PLATFORM" ]; then
        docker build --platform "$PLATFORM" "${tag_args[@]}" -f "$dockerfile" "$ROOT_DIR"
    else
        docker build "${tag_args[@]}" -f "$dockerfile" "$ROOT_DIR"
    fi

    if [ "$PUSH" = true ]; then
        for tag in "${TAGS[@]}"; do
            docker push "$REGISTRY/$name:$tag"
        done
    fi
}

for binary in fluidbg-operator fluidbg-http fluidbg-rabbitmq; do
    if [ ! -x "$ROOT_DIR/dist/$binary" ]; then
        echo "missing dist/$binary; run ./scripts/build-linux-binaries.sh first" >&2
        exit 1
    fi
done

build_image fbg-operator "$ROOT_DIR/Dockerfile"
build_image fbg-plugin-http "$ROOT_DIR/plugins/http/Dockerfile"
build_image fbg-plugin-rabbitmq "$ROOT_DIR/plugins/rabbitmq/Dockerfile"
