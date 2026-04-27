---
title: SDK Contract
---

# SDK Contract

The canonical plugin contract is JSON over HTTP and is versioned independently
from plugin implementations.

| Contract | Version | Location |
|---|---|---|
| Plugin API | `fluidbg.plugin/v1alpha1` | `sdk/spec/plugin-api-v1alpha1.openapi.yaml` |
| CRDs | `fluidbg.io/v1alpha1` | `crds/` and `sdk/spec/crd-versions.yaml` |
| Rust SDK | crate workspace member | `sdk/rust` |

Rust plugins should import shared models from `fluidbg-plugin-sdk`. Other
language SDKs should be generated from the OpenAPI spec so payloads remain
consistent.

```sh
openapi-generator-cli generate \
  -i sdk/spec/plugin-api-v1alpha1.openapi.yaml \
  -g go \
  -o sdk/generated/go
```

Generated SDKs are build artifacts and should be published per language package
ecosystem when their maintenance guarantees are clear.

