---
layout: home
title: FluidBG Operator
---

FluidBG is a Kubernetes operator for blue-green and progressive delivery where
candidate applications are validated with live queue or HTTP traffic before they
are promoted.

## Start Here

| Goal | Start Here |
|---|---|
| Try FluidBG locally | [Getting Started](getting-started.md), then [Sequential Example](examples/sequential-bgd.md) |
| Install or operate a cluster | [Operations](operations/index.md), then [Helm Installation](operations/helm.md) |
| Understand the system | [Reference](reference/index.md), then [Architecture](reference/architecture.md) |
| Build a plugin | [Plugin Architecture](reference/plugin-architecture.md), [Plugin Interface](reference/plugin-interface.md), and [SDK Contract](reference/sdk.md) |
| Diagnose a rollout | [Troubleshooting](operations/troubleshooting.md) and [E2E Test Flow](testing/e2e.md) |

## Documentation Map

| Area | Contents |
|---|---|
| [Tutorials](getting-started.md) | First install, local image loop, and runnable demo. |
| [Operations](operations/index.md) | Helm values, runtime configuration, troubleshooting, release verification, SBOMs, and signatures. |
| [Reference](reference/index.md) | Architecture, CRDs, security model, plugin contract, SDK, and built-in plugin behavior. |
| [Testing](testing/e2e.md) | End-to-end suite topology, scenarios, cleanup assertions, and HA store modes. |
| [Project](project/changelog.md) | Changelog, contribution rules, and security reporting. |

## Source Of Truth

- CRD schema: Rust models in `operator/src/crd`, generated into [CRDs](reference/crds.md).
- Operator behavior: [Architecture](reference/architecture.md) and [Security Model](reference/security-model.md).
- Plugin protocol: [Plugin Interface](reference/plugin-interface.md) and [SDK Contract](reference/sdk.md).
- Built-in plugin behavior: [Built-In Plugins](reference/plugins/index.md).
- Installable runtime defaults: [Helm Installation](operations/helm.md).

## What Ships

- A Rust Kubernetes operator.
- Built-in RabbitMQ, Azure Service Bus, and HTTP inception plugins.
- Versioned CRDs under `fluidbg.io/v1alpha1`.
- A versioned plugin API under `fluidbg.plugin/v1alpha1`.
- A Rust plugin SDK plus OpenAPI specs for generated SDKs in other languages.
- A Helm chart for cluster-wide operator watching and cluster-scoped built-in plugin registrations.
