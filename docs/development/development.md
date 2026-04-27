---
title: Development
---

# Development

## Fast Loop

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

## Image Loop

```sh
./scripts/build-linux-binaries.sh
./scripts/build-images.sh --tag dev
```

The runtime images are intentionally thin: each image contains only one stripped,
musl-linked release executable on a distroless static non-root base image.

Observed arm64 image sizes:

| Image | Size |
|---|---:|
| `fluidbg/operator` | 16.5 MB |
| `fluidbg/http` | 12.1 MB |
| `fluidbg/rabbitmq` | 13.9 MB |

## E2E Loop

```sh
KIND_CLUSTER=fluidbg-dev BUILD_IMAGES=1 ./e2e/run-test.sh
```

The e2e suite covers:

- hard-switch promotion
- rollback and queue-drain recovery
- progressive traffic shifting without restarting the splitter plugin pod
- unsupported progressive plugin rejection
- combined HTTP plugin proxy/observer behavior
