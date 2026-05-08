---
title: Operations
---

# Operations

Operations docs cover how to install, run, diagnose, and verify FluidBG in a
cluster. Runtime behavior details live in [Reference](../reference/index.md);
this section focuses on commands, values, and operational checks.

## Pages

| Page | Use It For |
|---|---|
| [Helm Installation](helm.md) | Chart values, operator auth, TLS, state stores, plugin managers, plugin registrations, and CRD upgrades. |
| [Troubleshooting](troubleshooting.md) | Status conditions, error reasons, role validation failures, and GitOps behavior. |
| [Release](release.md) | Release pipeline behavior, image signatures, attestations, SBOM verification, and artifact checks. |

## Common Tasks

| Task | Primary Page |
|---|---|
| Install the operator and built-in plugins | [Helm Installation](helm.md) |
| Decide which permissions BGD authors need | [Security Model](../reference/security-model.md) |
| Configure HA state storage | [Helm Installation](helm.md#state-store-and-ha) |
| Configure TLS between operator, plugins, and apps | [Helm Installation](helm.md#production-image-values) and [Plugin Interface](../reference/plugin-interface.md#control-plane-tls) |
| Verify signed images and SBOMs | [Release](release.md) |
| Understand a failed rollout | [Troubleshooting](troubleshooting.md) |
