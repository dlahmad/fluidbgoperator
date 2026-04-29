# FluidBG Operator

FluidBG is a Kubernetes operator for blue-green deployments where the candidate version is validated with live traffic before promotion. It wires transport-specific inception plugins around the candidate, records test cases through the operator API, polls the configured verifier container, and promotes or rolls back from the configured success criteria.

## Status

The repository is structured as a production-ready Rust workspace:

- operator and plugin crates build as stripped release executables
- runtime images are distroless and non-root
- CRDs are generated from Rust models and mirrored into the Helm chart
- CI covers formatting, clippy, unit tests, shell/Python checks, Docker builds, and Helm rendering
- scheduled/manual e2e runs execute the kind-based full rollout suite

## Workspace

```text
operator/              Rust operator crate and CRD generator
plugins/http/             Built-in HTTP observe/mock/write plugin
plugins/rabbitmq/         Built-in RabbitMQ multi-role plugin
plugins/azure_servicebus/ Built-in Azure Service Bus multi-role plugin
sdk/                   Versioned plugin SDK models and language-neutral OpenAPI spec
charts/                Helm chart for CRDs, operator, and built-in plugin CRs
docs/                  GitHub Pages/Jekyll documentation source
crds/                  Generated CRD manifests
builtin-plugins/       Standalone InceptionPlugin manifest examples
deploy/                Operator deployment and RBAC
e2e/                   Kind-based end-to-end scenario assets
testenv/               Local RabbitMQ/Postgres/kind manifests
```

The current controller code is split by responsibility:

```text
operator/src/controller.rs                 Reconcile phase machine
operator/src/controller/plugin_lifecycle.rs Plugin lifecycle HTTP protocol
operator/src/controller/promotion.rs        Promotion validation and decisions
operator/src/controller/resources.rs        Kubernetes resource apply/delete helpers
operator/src/controller/status.rs           Status patch helpers
```

## Build And Test

```sh
just check
```

Equivalent raw commands:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Build optimized release binaries and images:

```sh
just build-binaries
just build-images dev
```

End-to-end tests are implemented as a Rust `kube-rs` integration-test crate.
Scenario modules are under `e2e/src/scenarios/` and each scenario's manifests
are grouped under the matching `e2e/deploy/<scenario>/` folder. The wrapper
below requires Docker, kind, Helm, kubectl for port-forward/exec boundaries,
and local images:

```sh
KIND_CLUSTER=fluidbg-dev BUILD_IMAGES=1 ./e2e/run-test.sh
```

HA state-store e2e uses Postgres and two operator replicas:

```sh
KIND_CLUSTER=fluidbg-dev BUILD_IMAGES=1 E2E_STATE_STORE=postgres OPERATOR_REPLICAS=2 ./e2e/run-test.sh
```

## Install

Run local chart commands from the repository root. The `./` prefix matters:
without it, Helm can interpret `charts/fluidbg-operator` as a repository chart
reference instead of a local path.

```sh
helm upgrade --install fluidbg ./charts/fluidbg-operator \
  --namespace fluidbg-system \
  --create-namespace
```

The operator watches `BlueGreenDeployment` resources cluster-wide by default.
Because `InceptionPlugin` resources are namespaced, install built-in plugin CRs
into every application namespace that should use the chart-provided plugins:

```sh
helm upgrade --install fluidbg ./charts/fluidbg-operator \
  --namespace fluidbg-system \
  --create-namespace \
  --set builtinPlugins.namespaces='{fluidbg-system,my-app-namespace}'
```

## Images

Published release image names are:

- `ghcr.io/dlahmad/fbg-operator`
- `ghcr.io/dlahmad/fbg-plugin-http`
- `ghcr.io/dlahmad/fbg-plugin-rabbitmq`
- `ghcr.io/dlahmad/fbg-plugin-azure-servicebus`

Release builds use musl static linking, `strip`, thin LTO, single codegen unit, and `panic=abort`. Runtime containers contain only the compiled executable on a distroless static non-root base. Release amd64 and arm64 binaries are built on native GitHub-hosted Linux runners instead of compiling under emulation.

Observed release image sizes, measured as compressed registry transfer size:

| Image | linux/amd64 | linux/arm64 |
|---|---:|---:|
| `ghcr.io/dlahmad/fbg-operator` | 6.0 MB | 5.7 MB |
| `ghcr.io/dlahmad/fbg-plugin-http` | 3.5 MB | 3.3 MB |
| `ghcr.io/dlahmad/fbg-plugin-rabbitmq` | 4.2 MB | 4.0 MB |
| `ghcr.io/dlahmad/fbg-plugin-azure-servicebus` | 3.6 MB | 3.4 MB |

## Documentation

- Published docs are served from GitHub Pages once the `CI/CD` workflow docs job succeeds on `main`: <https://dlahmad.github.io/fluidbgoperator/>.
- [docs/index.md](docs/index.md) is the GitHub Pages entry point.
- [docs/getting-started.md](docs/getting-started.md) covers local setup, image builds, and e2e execution.
- [docs/reference/architecture.md](docs/reference/architecture.md) describes the operator model, CRDs, state store, plugin orchestration, and project layout.
- [docs/reference/plugin-interface.md](docs/reference/plugin-interface.md) defines the runtime contract between the operator, plugins, application deployments, and the verifier container.
- [docs/reference/plugins/index.md](docs/reference/plugins/index.md) links the formal per-plugin references for HTTP, RabbitMQ, and Azure Service Bus.
- [docs/operations/helm.md](docs/operations/helm.md) documents Helm installation and namespaced built-in plugin CRs.
- [docs/operations/release.md](docs/operations/release.md) documents tag/manual releases, GHCR images, and the OCI Helm chart.
- [docs/reference/sdk.md](docs/reference/sdk.md) documents the SDK layout and language-neutral spec.
- [docs/reference/crds.md](docs/reference/crds.md) documents CRD generation and chart mirroring.
- [docs/development/implementation-plan.md](docs/development/implementation-plan.md) tracks the current implementation state and near-term work.
- `docs/` is the GitHub Pages/Jekyll source for online documentation.
