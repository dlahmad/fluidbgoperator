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
    local context="$2"
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
        docker buildx build --load --platform "$PLATFORM" "${cache_args[@]}" "${tag_args[@]}" "$ROOT_DIR/$context"
    else
        docker buildx build --load "${cache_args[@]}" "${tag_args[@]}" "$ROOT_DIR/$context"
    fi

    if [ "$PUSH" = true ]; then
        for tag in "${TAGS[@]}"; do
            docker push "$REGISTRY/$name:$tag"
        done
    fi
}

build_image fluidbg-example-order-app examples/sequential-bgd/app
build_image fluidbg-example-producer examples/sequential-bgd/producer
build_image fluidbg-example-sink examples/sequential-bgd/sink
build_image fluidbg-example-verifier examples/sequential-bgd/verifier
