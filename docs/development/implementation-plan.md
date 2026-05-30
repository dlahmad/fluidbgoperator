---
title: Implementation Plan
---

# FluidBG Operator Implementation Plan

This document tracks the current implementation state and remaining work. The
Rust CRD models, SDK models, plugin manifests, and workflows are the source of
truth; the architecture and plugin-interface docs describe that implementation.

## Current Layout

```text
operator/              Operator crate, CRDs, controller, state stores, HTTP API
plugins/http/          Combined HTTP proxy/splitter/observer/mock/writer plugin
plugins/rabbitmq/      Combined RabbitMQ plugin roles
sdk/                   Plugin SDK models and language-neutral API specs
crds/                  Generated Kubernetes CRDs
builtin-plugins/       Standalone InceptionPlugin manifest examples
deploy/                Operator deployment and RBAC
e2e/                   Kind-based end-to-end test harness and apps
testenv/               RabbitMQ, Postgres, and kind manifests
```

The controller is intentionally split by concern:

```text
controller.rs                  Reconcile phase machine
controller/plugin_lifecycle.rs Plugin prepare/activate/drain/cleanup HTTP calls
controller/promotion.rs        Promotion validation and decision logic
controller/resources.rs        Kubernetes resource construction and deletion
controller/status.rs           BlueGreenDeployment status patches
```

## Implemented

| Area | Status |
|---|---|
| Rust workspace | Operator, Rust plugin SDK, combined HTTP, RabbitMQ, NATS JetStream, and Azure Service Bus plugin crates |
| CRDs | Versioned `fluidbg.io/v1alpha1` `BlueGreenDeployment` and `InceptionPlugin` |
| State stores | In-memory, PostgreSQL, and Azure Cosmos DB backends |
| Promotion strategies | Hard-switch and progressive strategy implementations |
| Plugin model | Generic plugin CRD rendering plus built-in combined HTTP, RabbitMQ, NATS JetStream, and Azure Service Bus manifests |
| Operator API | `/health`, `/testcases`, `/testcase-verdicts`, and `/counts/{bg_ref}` with BGD identity derived from authenticated token claims |
| Test harness | Unit tests plus a Rust/kube-rs kind-based e2e harness |
| Packaging | Helm chart, consolidated GitHub Actions CI/CD workflow, docs publishing, optional e2e gate, GHCR release targets, image signatures, SBOMs, and provenance attestations |

## Near-Term Work

1. Expand plugin contract tests that exercise `/prepare`, `/activate`, `/drain`, `/drain-status`, `/cleanup`, `/traffic`, and returned assignments outside the full e2e suite.
2. Add focused integration tests for PostgreSQL and Cosmos DB migration/recovery paths without requiring the full rollout scenario.
3. Add more failure-injection tests around plugin manager outages, API-server conflicts, and forced-delete orphan cleanup.
4. Keep release tooling current as GitHub Actions and Sigstore tooling move to newer runtime versions.

## Local Verification

```sh
cargo fmt --all --check
cargo test --workspace
cargo test -p fluidbg-e2e-tests --test e2e -- --ignored --test-threads=1 --nocapture
```

`./e2e/run-test.sh` runs the same command. The e2e harness requires Helm and
kubeconfig access to the target cluster. Local runs with `BUILD_IMAGES=1` also
require Docker and kind; with `BUILD_IMAGES=0`, the required local images must
already be loaded into the kind cluster.
