---
layout: page
title: HTTP Plugin Capabilities
---

# HTTP Plugin Capabilities

This page describes the supported HTTP plugin modes, proxy fidelity, and TLS
configuration.

## Modes And Roles

| Role selection | What it does | Forwards real HTTP | Observes test cases | Mocks responses | Supports `/write` | Progressive shifting | Body handling |
|---|---|---:|---:|---:|---:|---:|---|
| `observer` | Proxies HTTP requests, filters matching traffic, extracts `testId`, notifies the verifier, and registers blue/both/unknown cases with the operator. | yes | yes | no | no | no | Buffers up to 10 MiB when body fields are used. |
| `splitter` | Routes each request to `greenEndpoint` or `blueEndpoint` according to the current traffic percentage. | yes | no | no | no | yes | Buffers up to 10 MiB because routing currently hashes the request body. |
| `splitter` + `observer` | Weighted HTTP proxy plus observation. | yes | yes | no | no | yes | Buffers up to 10 MiB. |
| `mock` | For matched requests with `mockPath`, forwards the call to the verifier mock endpoint and returns the verifier response to the original caller. | unmatched or non-mock matched requests | no | yes | no | no | Streams when no body-based filter/test id is configured. |
| `observer` + `mock` | Notifies the verifier, then mocks only filters with `mockPath`; observer-only filters still proxy to the real upstream. | non-mock matched requests | yes | yes | no | no | Buffers if observer config needs the body. |
| `writer` | Exposes `POST /write` and forwards verifier-initiated HTTP calls to `targetUrl` or `realEndpoint`. | via `/write` only | no | no | yes | no | JSON writer API is buffered by design. |
| `observer` + `writer` | Proxies/observes application calls and exposes `/write`. | yes | yes | no | yes | no | Depends on observer config. |
| `splitter` + `observer` + `writer` | Progressive HTTP proxy, observation, and verifier-triggered writes in one inceptor. | yes | yes | no | yes | yes | Splitter path is bounded-buffered. |

## HTTP And TLS Support

| Capability | Current support | Details |
|---|---:|---|
| Incoming plain HTTP listener | yes | The plugin always listens on `0.0.0.0:<port>`, default `9090`, for control/lifecycle and optionally traffic. |
| Incoming HTTPS traffic listener | yes, optional | Configure `tls.inbound.enabled: true`; default HTTPS port is `9443`. The app/verifier can be injected with `https://...` using `proxyProtocol: https` or `writeProtocol: https`. |
| Control/lifecycle API over HTTP | yes | The operator continues to call `/prepare`, `/activate`, `/drain`, `/drain-status`, `/cleanup`, and `/traffic` on the HTTP listener. |
| Outbound HTTP upstreams | yes | `realEndpoint`, `greenEndpoint`, `blueEndpoint`, `targetUrl`, and `verifierEndpoint` can use `http://`. |
| Outbound HTTPS upstreams | yes | The plugin uses Rustls through `reqwest`; `https://` upstreams work with public WebPKI roots by default. |
| Private/internal CA trust | yes | Mount a CA file and set `clientTls.caCertPath`. |
| Disable outbound certificate verification | yes, unsafe | `clientTls.insecureSkipVerify: true` exists only for throwaway local tests. Do not use it for production. |
| Original HTTP method forwarding | yes | The proxy uses the incoming request method. |
| Query string forwarding | yes | The full path and query are forwarded. Filters still treat `http.path` as the path without query and expose query values through `http.query.<name>`. |
| Request header forwarding | partial by design | End-to-end headers are forwarded. Hop-by-hop headers, `host`, and `content-length` are skipped. |
| Response status forwarding | yes | Upstream or verifier status is returned to the caller. |
| Response header forwarding | partial by design | End-to-end response headers are forwarded. Hop-by-hop headers, `content-length`, and `transfer-encoding` are skipped and recalculated by the HTTP stack. |
| Response body forwarding | yes, streamed | Upstream response bodies are streamed back to the caller. |
| Request body streaming | yes where safe | Pure proxy and mock paths stream request bodies when no body-based routing/filtering/test-id extraction is required. |
| Request body inspection | bounded | Observer and splitter paths that need body data buffer up to 10 MiB. |
| Verifier authorization | yes | Observer notifications, mock calls, operator result polling, and verifier `/write` calls use the per-inception bearer token. |
| WebSocket or HTTP `CONNECT` tunneling | no | The plugin is an application-level HTTP proxy, not a transparent tunnel. |
| Transparent wire-level HTTP/2 pass-through | no | The plugin terminates the incoming HTTP request and creates a new upstream request. |

## Configuration Fields

| Field | Used by | Meaning |
|---|---|---|
| `port` | all roles | HTTP control/traffic listener port. Defaults to `9090`. |
| `proxyProtocol` | operator injection | `http` or `https` for `envVarName` injection. Defaults to `https` when inbound TLS is enabled, otherwise `http`. |
| `writeProtocol` | operator injection | `http` or `https` for `writeEnvVar` injection. Defaults to `proxyProtocol`. |
| `realEndpoint` | proxy/write fallback | Default upstream target. Supports `http://`, `https://`, `{testContainerUrl}`, and `{{testContainerUrl}}`. |
| `greenEndpoint` | `splitter` | Explicit green route target. Falls back to `realEndpoint`. |
| `blueEndpoint` | `splitter` | Explicit blue/candidate route target. Falls back to `realEndpoint`. |
| `targetUrl` | `writer` | Explicit `/write` target. Falls back to `realEndpoint`. |
| `verifierEndpoint` | `observer`, `mock` | Optional verifier/test-container base URL override. Falls back to operator-provided `FLUIDBG_TEST_CONTAINER_URL`. |
| `mockPath` | `mock` | Default verifier path used for mock responses. Supports `{testId}` and `{inceptionPoint}`. |
| `filters[].mockPath` | `mock` | Filter-specific verifier mock path. Overrides top-level `mockPath`. |
| `envVarName` | operator assignment | Application env var patched to route calls through the HTTP inceptor service. |
| `writeEnvVar` | operator assignment | Test-container env var patched with the plugin `/write` URL. |
| `testId` | `observer`, `mock` | Selector used to extract a test id from body, path, header, or static value. |
| `match` | `observer`, `mock` | Root filter set. All conditions must match. |
| `filters` | `observer`, `mock` | Filter-specific match rules, `notifyPath`, `mockPath`, and payload selection. |
| `tls.inbound.enabled` | app/verifier to plugin | Enables the additional HTTPS traffic listener. |
| `tls.inbound.port` | app/verifier to plugin | HTTPS listener port. Defaults to `9443`. |
| `tls.inbound.certPath` | app/verifier to plugin | Mounted PEM certificate path. Required when inbound TLS is enabled. |
| `tls.inbound.keyPath` | app/verifier to plugin | Mounted PEM private key path. Required when inbound TLS is enabled. |
| `clientTls.caCertPath` | plugin to upstream/verifier | Mounted PEM CA bundle used to trust private upstream/verifier certificates. |
| `clientTls.insecureSkipVerify` | plugin to upstream/verifier | Disables outbound certificate verification. Local test use only. |

## Private CA Example

The operator does not create TLS certificates. Mount existing Kubernetes
Secrets/ConfigMaps into generated HTTP inceptor pods through the global
`InceptionPlugin` registration. With the Helm chart, configure the built-in HTTP
plugin once:

```yaml
builtinPlugins:
  http:
    enabled: true
    inceptorVolumes:
      - name: http-inceptor-tls
        secret:
          secretName: fluidbg-http-inceptor-tls
      - name: private-ca
        configMap:
          name: private-ca
    inceptorVolumeMounts:
      - name: http-inceptor-tls
        mountPath: /tls
        readOnly: true
      - name: private-ca
        mountPath: /ca
        readOnly: true
```

Then reference those mounted files from the BGD inception point:

```yaml
config:
  envVarName: HTTP_UPSTREAM
  realEndpoint: https://orders-api.default.svc.cluster.local
  proxyProtocol: https
  tls:
    inbound:
      enabled: true
      port: 9443
      certPath: /tls/tls.crt
      keyPath: /tls/tls.key
  clientTls:
    caCertPath: /ca/ca.crt
```

The Secret and ConfigMap are namespace-local resources because the generated
inceptor pod runs in the same namespace as the `BlueGreenDeployment`. If BGDs in
multiple namespaces use HTTPS, create equivalent Secret/ConfigMap names in each
namespace or register a separate plugin with different mount names.
