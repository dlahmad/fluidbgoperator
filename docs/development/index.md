---
title: Development
---

# Development

Development docs are for contributors changing the operator, SDK, plugins, or
test harness.

## Pages

| Page | Use It For |
|---|---|
| [Development](development.md) | Fast local loops, image builds, and e2e commands. |
| [Implementation Plan](implementation-plan.md) | Current layout, implemented areas, and near-term cleanup ideas. |
| [E2E Test Flow](../testing/e2e.md) | Full cluster scenario structure and cleanup expectations. |
| [Contributing](../project/contributing.md) | Required checks and code standards. |

## Required Checks

Run these before pushing code changes:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

For operator/plugin behavior changes, also run the ignored Kind e2e suite:

```sh
KIND_CLUSTER=<cluster> BUILD_IMAGES=1 ./e2e/run-test.sh
```
