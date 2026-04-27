---
title: SDK Specs
---

# FluidBG Plugin SDK Specs

This directory contains the language-neutral plugin contract. The Rust SDK in
`sdk/rust` implements the same model directly; other SDKs should be generated
from `plugin-api-v1alpha1.openapi.yaml`.

## Versioning

| Contract | Version |
|---|---|
| Plugin API | `fluidbg.plugin/v1alpha1` |
| Kubernetes CRD group | `fluidbg.io` |
| Kubernetes CRD version | `v1alpha1` |

## SDK Generation

The canonical plugin wire contract is OpenAPI because FluidBG plugins already
communicate with the operator and test containers through JSON over HTTP. This
keeps non-Rust SDKs generated from one source of truth without forcing plugin
authors into a specific runtime. Protocol Buffers would also support many
languages, but it would add an RPC/binary-schema layer that does not match the
current HTTP plugin contract.

Use OpenAPI Generator for client/server SDKs in common languages:

```sh
openapi-generator-cli generate -i sdk/spec/plugin-api-v1alpha1.openapi.yaml -g python -o sdk/generated/python
openapi-generator-cli generate -i sdk/spec/plugin-api-v1alpha1.openapi.yaml -g typescript-fetch -o sdk/generated/typescript
openapi-generator-cli generate -i sdk/spec/plugin-api-v1alpha1.openapi.yaml -g go -o sdk/generated/go
openapi-generator-cli generate -i sdk/spec/plugin-api-v1alpha1.openapi.yaml -g java -o sdk/generated/java
```

The stable wire contract is JSON over HTTP. Plugin-specific transport details
such as RabbitMQ queue names or HTTP upstream URLs remain plugin configuration,
but lifecycle, observation, and test registration payloads should stay aligned
with this spec.

Reference tooling:

- OpenAPI Generator generator list: https://openapi-generator.tech/docs/generators/
- Protocol Buffers language support overview: https://protobuf.dev/overview/
