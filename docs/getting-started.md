---
title: Getting Started
---

# Getting Started

## Prerequisites

- Kubernetes v1.30 or newer with permission to install CRDs and cluster RBAC.
  This is the minimum supported version for the default Helm installation
  because FluidBG installs `ValidatingAdmissionPolicy` resources.
- `helm` for chart installation.
- `kubectl` for the manual commands shown in the docs and examples.
- Docker and kind for local e2e/example development and local image loading.

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
./scripts/build-example-images.sh --registry fluidbg --tag dev
```

`build-linux-binaries.sh` builds Linux musl static executables inside Docker by
default so the produced Docker images are valid even when invoked from macOS.
Example images are local demo/test images only; the release pipeline does not
publish them to GHCR.

For a local kind cluster:

```sh
kind load docker-image fluidbg/fbg-operator:dev --name fluidbg-dev
kind load docker-image fluidbg/fbg-plugin-http:dev --name fluidbg-dev
kind load docker-image fluidbg/fbg-plugin-rabbitmq:dev --name fluidbg-dev
kind load docker-image fluidbg/fbg-plugin-azure-servicebus:dev --name fluidbg-dev
kind load docker-image fluidbg/fluidbg-example-order-app:dev --name fluidbg-dev
kind load docker-image fluidbg/fluidbg-example-producer:dev --name fluidbg-dev
kind load docker-image fluidbg/fluidbg-example-sink:dev --name fluidbg-dev
kind load docker-image fluidbg/fluidbg-example-verifier:dev --name fluidbg-dev
```

## Install With Helm

Run local chart commands from the repository root. The `./` prefix matters:
without it, Helm can interpret `charts/fluidbg-operator` as a repository chart
reference instead of a local path.

```sh
helm upgrade --install fluidbg ./charts/fluidbg-operator \
  --namespace fluidbg-system \
  --create-namespace
```

Built-in plugin registrations are cluster-scoped. Install the chart once, then
reference `rabbitmq`, `http`, or `azure-servicebus` from
`BlueGreenDeployment` resources in any namespace the operator is allowed to
watch.

For production values, auth, state stores, TLS, and required BGD-author
permissions, continue with [Helm Installation](operations/helm.md) and
[Security Model](reference/security-model.md).

## Run E2E

The e2e suite is a Rust integration-test harness using `kube-rs` for Kubernetes
API operations. `e2e/run-test.sh` is a thin wrapper that runs the ignored full
cluster test.

```sh
KIND_CLUSTER=fluidbg-dev BUILD_IMAGES=1 ./e2e/run-test.sh
```

Run the HA state-store path with Postgres and two operator replicas:

```sh
KIND_CLUSTER=fluidbg-dev BUILD_IMAGES=1 E2E_STATE_STORE=postgres OPERATOR_REPLICAS=2 ./e2e/run-test.sh
```
