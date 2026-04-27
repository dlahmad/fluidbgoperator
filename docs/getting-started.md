---
title: Getting Started
---

# Getting Started

## Prerequisites

- Kubernetes cluster with permission to install CRDs and cluster RBAC.
- `kubectl`.
- `helm` for chart installation.
- Docker and kind for local e2e development.

## Local Checks

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

## Build Images

```sh
./scripts/build-linux-binaries.sh
./scripts/build-images.sh --tag dev
```

`build-linux-binaries.sh` builds Linux musl static executables inside Docker by
default so the produced Docker images are valid even when invoked from macOS.

For a local kind cluster:

```sh
kind load docker-image fluidbg/operator:dev --name fluidbg-dev
kind load docker-image fluidbg/http:dev --name fluidbg-dev
kind load docker-image fluidbg/rabbitmq:dev --name fluidbg-dev
```

## Install With Helm

```sh
helm upgrade --install fluidbg charts/fluidbg-operator \
  --namespace fluidbg-system \
  --create-namespace
```

If your `BlueGreenDeployment` resources live in application namespaces, install
built-in plugin CRs into those namespaces too:

```sh
helm upgrade --install fluidbg charts/fluidbg-operator \
  --namespace fluidbg-system \
  --create-namespace \
  --set builtinPlugins.namespaces='{fluidbg-system,my-app-namespace}'
```

## Run E2E

```sh
KIND_CLUSTER=fluidbg-dev BUILD_IMAGES=1 ./e2e/run-test.sh
```
