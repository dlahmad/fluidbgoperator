---
layout: home
title: FluidBG Operator
---

FluidBG is a Kubernetes operator for blue-green and progressive delivery where
candidate applications are validated with live queue or HTTP traffic before they
are promoted.

## Start Here

- [Getting Started](getting-started.md)
- [Helm Installation](operations/helm.md)
- [Architecture](reference/architecture.md)
- [Plugin Architecture](reference/plugin-architecture.md)
- [Plugin Interface](reference/plugin-interface.md)
- [SDK Contract](reference/sdk.md)
- [E2E Test Flow](testing/e2e.md)
- [Development](development/development.md)
- [Implementation Plan](development/implementation-plan.md)
- [Sequential Example](examples/sequential-bgd.md)
- [Changelog](project/changelog.md)
- [Contributing](project/contributing.md)
- [Security Policy](project/security.md)

## What Ships

- A Rust Kubernetes operator.
- Built-in RabbitMQ, Azure Service Bus, and HTTP inception plugins.
- Versioned CRDs under `fluidbg.io/v1alpha1`.
- A versioned plugin API under `fluidbg.plugin/v1alpha1`.
- A Rust plugin SDK plus OpenAPI specs for generated SDKs in other languages.
- A Helm chart for installing the operator and namespaced built-in plugin CRs.
