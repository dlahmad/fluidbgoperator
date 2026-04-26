# FluidBG Operator Implementation Plan

This document tracks the current implementation state and remaining work. `ARCHITECTURE.md` is the source of truth for the system model; `PLUGIN.md` is the source of truth for the plugin runtime contract.

## Current Layout

```text
operator/              Operator crate, CRDs, controller, state stores, HTTP API
plugins/http_proxy/    HTTP proxy plugin
plugins/http_writer/   HTTP writer plugin
plugins/rabbitmq/      Combined RabbitMQ plugin roles
crds/                  Generated Kubernetes CRDs
builtin-plugins/       InceptionPlugin manifests
deploy/                Operator deployment and RBAC
e2e/                   Kind-based end-to-end test harness and apps
testenv/               RabbitMQ, Postgres, and kind manifests
```

The controller is intentionally split by concern:

```text
controller.rs                  Reconcile phase machine
controller/plugin_lifecycle.rs Plugin prepare/drain/cleanup HTTP calls
controller/promotion.rs        Promotion validation and decision logic
controller/resources.rs        Kubernetes resource construction and deletion
controller/status.rs           BlueGreenDeployment status patches
```

## Implemented

| Area | Status |
|---|---|
| Rust workspace | Operator plus HTTP and RabbitMQ plugin crates |
| CRDs | `BlueGreenDeployment`, `InceptionPlugin`, `StateStore` |
| State stores | In-memory and PostgreSQL backends |
| Promotion strategies | Hard-switch and progressive strategy implementations |
| Plugin model | Generic plugin CRD rendering plus built-in HTTP/RabbitMQ manifests |
| Operator API | `/health`, `/testcases`, `/testcase-verdicts`, `/counts/{bg_ref}` |
| Test harness | Unit tests plus kind-based e2e assets |

## Near-Term Work

1. Expand controller unit tests around draining finalization, generated candidate name collisions, and env assignment grouping.
2. Add integration tests for PostgreSQL state-store migration and recovery.
3. Add plugin contract tests that exercise `/prepare`, `/drain`, `/drain-status`, `/cleanup`, and returned assignments.
4. Decide whether Redis remains on the roadmap; if yes, add the backend and document its operational model before advertising it as supported.
5. Add CI jobs for `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and a gated kind e2e workflow.

## Local Verification

```sh
cargo fmt --all --check
cargo test --workspace
./e2e/run-test.sh
```

The e2e script requires Docker, kind, kubectl, RabbitMQ/Postgres manifests from `testenv/`, and locally built images loaded into the kind cluster.
