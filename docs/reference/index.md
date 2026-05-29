---
title: Reference
---

# Reference

Reference pages are organized by responsibility. Start with the broad system
pages, then move into CRDs, plugins, or SDKs depending on what you are changing.

## System

| Page | Use It For |
|---|---|
| [Architecture](architecture.md) | Operator phases, reconciliation order, status behavior, failure behavior, and system diagrams. |
| [Security Model](security-model.md) | Admission policy, required BGD-author permissions, generated Secret handling, and cleanup trust boundaries. |
| [CRDs](crds.md) | API group/version, resource scope, CRD regeneration, and model-level rules. |

## Plugin Development

| Page | Use It For |
|---|---|
| [Plugin Architecture](plugin-architecture.md) | Manager/inceptor split, trust boundary, lifecycle flow, and plugin cleanup model. |
| [Plugin Interface](plugin-interface.md) | Lifecycle endpoints, auth headers, injected env vars, TLS, verifier contract, and notification bodies. |
| [SDK Contract](sdk.md) | Rust SDK exports and generated SDK expectations. |
| [SDK Specs](sdk-spec.md) | Language-neutral OpenAPI/versioning inputs for non-Rust SDK generation. |

## Built-In Plugins

| Page | Use It For |
|---|---|
| [Built-In Plugins](plugins/index.md) | Plugin reference index and common page template. |
| [RabbitMQ](plugins/rabbitmq.md) | Queue roles, RabbitMQ manager auth, queue declaration, drain, and cleanup behavior. |
| [NATS JetStream](plugins/nats.md) | Subject/stream roles, NATS manager auth, stream configuration, drain, and cleanup behavior. |
| [Azure Service Bus](plugins/azure-servicebus.md) | Service Bus roles, connection-string/workload-identity auth, queue declaration, drain, and cleanup behavior. |
| [HTTP](plugins/http.md) | HTTP proxy/splitter/observer/mock/writer behavior, TLS, drain, and security boundaries. |
| [HTTP Capabilities](plugins/http-capabilities.md) | Compact matrix of HTTP proxy modes, TLS support, and proxy limitations. |

## Reading Order

1. Read [Architecture](architecture.md) for the rollout state machine.
2. Read [Security Model](security-model.md) before granting permissions or
   installing in shared clusters.
3. Read [Plugin Architecture](plugin-architecture.md) before implementing or
   operating manager-backed plugins.
4. Read the relevant built-in plugin page for transport-specific behavior.
