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
plugins/http/          Combined HTTP observe/mock/write plugin
plugins/rabbitmq/      Combined RabbitMQ plugin roles
sdk/                   Plugin SDK models and language-neutral API specs
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
| Rust workspace | Operator, Rust plugin SDK, combined HTTP plugin, and RabbitMQ plugin crates |
| CRDs | Versioned `fluidbg.io/v1alpha1` `BlueGreenDeployment` and `InceptionPlugin` |
| State stores | In-memory, PostgreSQL, and Azure Cosmos DB backends |
| Promotion strategies | Hard-switch and progressive strategy implementations |
| Plugin model | Generic plugin CRD rendering plus built-in combined HTTP/RabbitMQ manifests |
| Operator API | `/health`, `/testcases`, `/testcase-verdicts`, `/counts/{bg_ref}` |
| Test harness | Unit tests plus kind-based e2e assets |
| Packaging | Helm chart, GitHub Actions CI/docs/e2e/release workflows, GHCR release targets |

## Near-Term Work

1. Expand controller unit tests around draining finalization and env assignment grouping.
2. Add integration tests for PostgreSQL state-store migration and recovery.
3. Add standalone plugin contract tests that exercise `/prepare`, `/drain`, `/drain-status`, `/cleanup`, `/traffic`, and returned assignments outside the e2e suite.
4. Decide whether Redis remains on the roadmap; if yes, add the backend and document its operational model before advertising it as supported.
5. Add release hardening for signed images, SBOMs, provenance attestations, and package retention policy.
6. Migrate JavaScript-based GitHub Actions to Node 24-compatible action versions as they become available.

## Local Verification

```sh
cargo fmt --all --check
cargo test --workspace
./e2e/run-test.sh
```

The e2e script requires Docker, kind, kubectl, RabbitMQ/Postgres manifests from `testenv/`, and locally built images loaded into the kind cluster.
