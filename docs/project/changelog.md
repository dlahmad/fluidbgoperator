---
title: Changelog
---

# Changelog

## 0.2.3 - 2026-05-08

- Added Cargo-aware CycloneDX SBOM generation for every released Rust binary
  and architecture.
- Attached the Rust dependency SBOMs to GitHub Releases and attested them
  against the published GHCR image manifests.
- Documented how to inspect BuildKit filesystem SBOMs, Rust dependency SBOMs,
  and GitHub image attestations from the CLI.

## 0.2.2 - 2026-05-08

- Added BuildKit SPDX SBOM and SLSA provenance attestations for pushed release
  images.
- Updated Rust dependencies to the latest compatible lockfile versions.
- Made default logging filters component-scoped so dependency logs stay quiet at
  the default level.
- Moved per-reconcile, per-test-case, and cleanup-wait chatter from `info` to
  `debug`; lifecycle transitions, rollbacks, drain movement summaries, and
  failures remain visible by default.
- Added Helm values for operator and built-in plugin `RUST_LOG` overrides.

## 0.2.1 - 2026-05-08

- Refactored the Rust e2e harness to remove Python assumptions and use kube-rs
  for in-cluster HTTP calls, pod exec, and RabbitMQ/Postgres test interactions.
- Enabled the kube websocket feature and Tokio IO utilities needed by the
  programmatic Kubernetes API test harness.
- Updated e2e and example documentation to clarify required local tools and the
  Kubernetes API based test flow.

## 0.2.0 - 2026-05-08

- Kept queue-specific temporary name derivation inside queue plugin managers.
- Added manager-returned effective inceptor config to the plugin SDK contract.
- Removed unused operator-side queue filter/selector helper modules.
- Updated plugin architecture docs to state that the operator treats transport config as opaque plugin data.
- Fixed queue plugin role composition so additive `observer` and `writer` roles no longer disable splitter, duplicator, combiner, or consumer workers.
- Added plugin-declared role constraints and consistent BGD status diagnostics for invalid specs and reconcile failures.

## 0.1.8 - 2026-04-29

- Added Rust plugin SDK and versioned OpenAPI plugin contract.
- Combined HTTP plugin into one built-in plugin.
- Added progressive splitter traffic shifting support.
- Added Helm chart for operator, CRDs, RBAC, service, and built-in plugin CRs.
- Added and then consolidated CI, e2e, release, image, and docs automation into one guarded CI/CD workflow.
- Added optimized release profile and distroless runtime image build scripts.
- Fixed progressive promotion so continuous traffic does not create a moving-tail blocker after the configured finalized sample has passed.
- Added drain protection for already-registered pending cases and namespace-deletion cleanup coverage.
