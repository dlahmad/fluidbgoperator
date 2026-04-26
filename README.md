# FluidBG Operator

FluidBG is a Kubernetes operator for blue-green deployments where the candidate version is validated with live traffic before promotion. It wires transport-specific inception plugins around the candidate, records test cases through the operator API, polls verifier containers, and promotes or rolls back from the configured success criteria.

## Workspace

```text
operator/              Rust operator crate and CRD generator
plugins/http_proxy/    Built-in HTTP proxy plugin
plugins/http_writer/   Built-in HTTP writer plugin
plugins/rabbitmq/      Built-in RabbitMQ multi-role plugin
crds/                  Generated CRD manifests
builtin-plugins/       InceptionPlugin manifests for shipped plugins
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
cargo fmt --all --check
cargo test --workspace
```

End-to-end tests require Docker, kind, kubectl, and local images:

```sh
./e2e/run-test.sh
```

Useful approved local workflows in this repository also include `docker build`, `kind load docker-image`, and `kubectl` inspection for the kind cluster.

## Documentation

- `ARCHITECTURE.md` describes the operator model, CRDs, state store, plugin orchestration, and project layout.
- `PLUGIN.md` defines the runtime contract between the operator, plugins, application deployments, and verifier containers.
- `IMPLEMENTATION_PLAN.md` tracks the current implementation state and near-term work.
