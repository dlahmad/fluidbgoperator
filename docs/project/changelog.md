---
title: Changelog
---

# Changelog

## 0.1.8 - 2026-04-29

- Added Rust plugin SDK and versioned OpenAPI plugin contract.
- Combined HTTP plugin into one built-in plugin.
- Added progressive splitter traffic shifting support.
- Added Helm chart for operator, CRDs, RBAC, service, and built-in plugin CRs.
- Added and then consolidated CI, e2e, release, image, and docs automation into one guarded CI/CD workflow.
- Added optimized release profile and distroless runtime image build scripts.
- Fixed progressive promotion so continuous traffic does not create a moving-tail blocker after the configured finalized sample has passed.
- Added drain protection for already-registered pending cases and namespace-deletion cleanup coverage.
