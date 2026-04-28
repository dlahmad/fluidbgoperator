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

The [SDK Specs](sdk-spec.md) page explains the language-neutral OpenAPI setup
and generation targets.

Rust plugins should import shared models from `fluidbg-plugin-sdk`. Other
language SDKs should be generated from the OpenAPI spec so payloads remain
consistent.

The Rust SDK also provides the shared auth primitives used by built-in plugins:

- `PLUGIN_AUTH_TOKEN_ENV` for `FLUIDBG_PLUGIN_AUTH_TOKEN`
- `AUTHORIZATION_HEADER` for bearer-token transport
- `PluginInceptorRuntime` for operator/test-container discovery inside an inceptor
- `PluginManagerLifecycleRequest` for manager prepare/cleanup requests
- `PluginLifecycleResponse` and `PluginDrainStatusResponse` for inceptor
  prepare, activate, drain, drain-status, and cleanup endpoints
- `PluginAuthClaims` for signed per-inception identity
- `derived_temp_queue_name_with_uid` for manager/inceptor agreement on derived queue names scoped by namespace, BGD name, BGD UID, inception point, role, and logical purpose
- `derived_temp_queue_name` for compatibility when no BGD UID is available
- `sign_plugin_auth_token` and `verify_plugin_auth_token` for HS256 JWT handling
- `bearer_value`, `bearer_token`, and `bearer_matches` for endpoint checks

Custom plugins should use the injected `FLUIDBG_PLUGIN_AUTH_TOKEN` unchanged
when calling the operator and should require the same bearer token on their
operator-owned lifecycle endpoints.
Managers should verify the bearer token signature with the operator signing key
and derive privileged resource names from verified claims, not from untrusted BGD
config fields.

```sh
openapi-generator-cli generate \
  -i sdk/spec/plugin-api-v1alpha1.openapi.yaml \
  -g go \
  -o sdk/generated/go
```

Generated SDKs are build artifacts and should be published per language package
ecosystem when their maintenance guarantees are clear.
