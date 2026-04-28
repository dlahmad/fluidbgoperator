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

    local cache_args=()
    if [ -n "${DOCKER_BUILD_CACHE_FROM:-}" ]; then
        cache_args+=("--cache-from" "$DOCKER_BUILD_CACHE_FROM-$name")
    fi
    if [ -n "${DOCKER_BUILD_CACHE_TO:-}" ]; then
        cache_args+=("--cache-to" "$DOCKER_BUILD_CACHE_TO-$name")
    fi

    if [ -n "$PLATFORM" ]; then
        docker buildx build --load --platform "$PLATFORM" "${cache_args[@]}" "${tag_args[@]}" -f "$dockerfile" "$ROOT_DIR"
    else
        docker buildx build --load "${cache_args[@]}" "${tag_args[@]}" -f "$dockerfile" "$ROOT_DIR"
    fi

    if [ "$PUSH" = true ]; then
        for tag in "${TAGS[@]}"; do
            docker push "$REGISTRY/$name:$tag"
        done
    fi
}

for binary in fluidbg-operator fluidbg-http fluidbg-rabbitmq fluidbg-azure-servicebus; do
    if [ ! -x "$ROOT_DIR/dist/$binary" ]; then
        echo "missing dist/$binary; run ./scripts/build-linux-binaries.sh first" >&2
        exit 1
    fi
done

build_image fbg-operator "$ROOT_DIR/Dockerfile"
build_image fbg-plugin-http "$ROOT_DIR/plugins/http/Dockerfile"
build_image fbg-plugin-rabbitmq "$ROOT_DIR/plugins/rabbitmq/Dockerfile"
build_image fbg-plugin-azure-servicebus "$ROOT_DIR/plugins/azure_servicebus/Dockerfile"
