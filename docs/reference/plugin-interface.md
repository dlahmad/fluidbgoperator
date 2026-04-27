---
title: Plugin Interface
---

# Plugin Interface

This document describes the runtime contract between:

- the operator
- standalone or sidecar plugins
- the test container
- the application deployments

It focuses on how plugins discover the other components, which HTTP calls are expected, and which values are injected by the operator.

## Overview

The operator does not hardcode transport behavior. A plugin is registered as an `InceptionPlugin` CR and selected by an `InceptionPoint` in a `BlueGreenDeployment`.

At runtime the operator is responsible for:

1. creating the plugin deployment or sidecar
2. calling plugin lifecycle endpoints
3. patching green, blue, and test deployments with plugin-provided assignments
4. injecting runtime URLs and identity into the plugin container
5. cleaning up plugin and test resources after promotion or rollback

The plugin is responsible for:

1. transport-specific setup during `prepare`
2. transport-specific cleanup during `cleanup`
3. observing, duplicating, splitting, combining, writing, or consuming traffic according to its active roles
4. registering `testCase`s with the operator when observation says a test-relevant event happened
5. notifying the test container when configured to do so

## Control Plane Contract

### Versioned SDK Contract

The plugin wire contract is versioned separately from plugin implementations:

| Contract | Version | Source |
|---|---|---|
| Plugin API | `fluidbg.plugin/v1alpha1` | `sdk/spec/plugin-api-v1alpha1.openapi.yaml` |
| Kubernetes CRDs | `fluidbg.io/v1alpha1` | `sdk/spec/crd-versions.yaml` |
| Rust SDK crate | `fluidbg-plugin-sdk` | `sdk/rust` |

Rust plugins should use `fluidbg-plugin-sdk` for shared lifecycle, assignment,
filter, selector, route, observation, and test registration models. Non-Rust
plugins should generate clients or server stubs from the OpenAPI spec so all
SDKs keep the same JSON shapes and endpoint semantics.

### InceptionPlugin CRD

An `InceptionPlugin` declares:

- `supportedRoles`
- `topology`
- `lifecycle.preparePath`
- `lifecycle.cleanupPath`
- `configSchema`
- `fieldNamespaces`
- `features`

For the built-in RabbitMQ and Azure Service Bus plugins the important roles are:

- `duplicator`
- `splitter`
- `combiner`
- `observer`
- `writer`
- `consumer`

### InceptionPoint

An `InceptionPoint` activates one or more roles on one plugin instance and provides transport-specific config.

Example:

```yaml
inceptionPoints:
  - name: incoming-orders
    pluginRef:
      name: rabbitmq
    roles: [duplicator, observer]
    config:
      amqpUrl: "amqp://fluidbg:fluidbg@rabbitmq.fluidbg-system:5672/%2f"
      duplicator:
        inputQueue: orders
        greenInputQueue: orders-green
        blueInputQueue: orders-blue
        greenInputQueueEnvVar: INPUT_QUEUE
        blueInputQueueEnvVar: INPUT_QUEUE
      observer:
        testId:
          field: queue.body
          jsonPath: $.orderId
        match:
          - field: queue.body
            jsonPath: $.type
            matches: "^order$"
        notifyPath: /observe/{testId}/incoming-orders
```

## Runtime Discovery

The plugin does not guess where the operator or test container are. The operator injects those values.

### Injected Plugin Environment

For standalone plugins the operator injects:

- `FLUIDBG_OPERATOR_URL`
  - example: `http://fluidbg-operator.fluidbg-system:8090`
- `FLUIDBG_TESTCASE_REGISTRATION_URL`
  - example: `http://fluidbg-operator.fluidbg-system:8090/testcases`
- `FLUIDBG_TEST_CONTAINER_URL`
  - example: `http://test-container.fluidbg-test:8080`
- `FLUIDBG_TESTCASE_VERIFY_PATH_TEMPLATE`
  - example: `/result/{testId}`
- `FLUIDBG_INCEPTION_POINT`
  - example: `incoming-orders`
- `FLUIDBG_BLUE_GREEN_REF`
  - example: `order-processor-bootstrap`
- `FLUIDBG_ACTIVE_ROLES`
  - example: `duplicator,observer`
- `FLUIDBG_CONFIG_PATH`
  - example: `/etc/fluidbg/config.yaml`
- `FLUIDBG_PLUGIN_AUTH_TOKEN`
  - per-inception JWT signed by the operator

Standalone env injection templates can also reference operator-provided template
context values such as `{{pluginServiceName}}`, `{{pluginDeploymentName}}`,
`{{inceptionPoint}}`, `{{blueGreenRef}}`, and `{{namespace}}`. Built-in HTTP
uses `{{pluginServiceName}}` so both blue and test containers can call the same
combined HTTP plugin service.

The plugin gets the full operator and test-container base URLs from env injection, not from hardcoded names inside the plugin itself.

## Operator and Plugin Authentication

The operator uses a user-selected Kubernetes Secret as its JWT signing key. The
Secret name and key are configured with:

- `FLUIDBG_AUTH_SIGNING_SECRET_NAME`
- `FLUIDBG_AUTH_SIGNING_SECRET_KEY`

For each inception point the operator signs one JWT and injects it as
`FLUIDBG_PLUGIN_AUTH_TOKEN`. The token claims identify the caller:

| Claim | Meaning |
|---|---|
| `iss` | Fixed issuer: `fluidbg-operator` |
| `aud` | Fixed audience: `fluidbg-inception-plugin` |
| `namespace` | BGD namespace watched by the operator |
| `blue_green_ref` | `BlueGreenDeployment.metadata.name` |
| `inception_point` | `InceptionPoint.name` |
| `plugin` | `InceptionPlugin.metadata.name` |

The same token is used in both directions:

- Operator to plugin lifecycle calls set `Authorization: Bearer <token>`.
- Plugin lifecycle endpoints reject requests whose bearer token does not match
  `FLUIDBG_PLUGIN_AUTH_TOKEN`.
- Plugin to operator `/testcases` calls set `Authorization: Bearer <token>`.
- The operator verifies the JWT signature and then derives the caller identity
  from claims. It rejects `/testcases` if the body `blue_green_ref` or
  `inception_point` differs from the verified token claims.

Only the signing Secret is long-lived. Per-inception JWTs are generated by the
operator and injected into plugin pods; they are not written to the state store.
Temporary plugin Deployments, Services, ConfigMaps, Pods, and legacy
per-inception auth Secrets are deleted after promotion, rollback, or rollout
restart cleanup. The selected operator signing Secret is user-owned and is not
deleted by rollout cleanup.

### How URLs Are Built

- The operator URL is a cluster service in `fluidbg-system`.
- The test container URL is namespace-qualified:
  - `http://<test-name>.<bgd-namespace>:<port>`
- The verify URL for each `testCase` is built by the plugin from:
  - `FLUIDBG_TEST_CONTAINER_URL`
  - `FLUIDBG_TESTCASE_VERIFY_PATH_TEMPLATE`

Example:

- base: `http://test-container.fluidbg-test:8080`
- verify path template: `/result/{testId}`
- final verify URL for `order-17`:
  - `http://test-container.fluidbg-test:8080/result/order-17`

## Lifecycle Endpoints

### `POST /prepare`

Called by the operator before observation starts.

Purpose:

- create transport-specific derived resources
- return property assignments for green, blue, and test targets

Response shape:

```json
{
  "assignments": [
    {
      "target": "green",
      "kind": "env",
      "name": "INPUT_QUEUE",
      "value": "orders-green"
    },
    {
      "target": "blue",
      "kind": "env",
      "name": "INPUT_QUEUE",
      "value": "orders-blue"
    }
  ]
}
```

Assignment fields:

| Field | Values | Meaning |
|---|---|---|
| `target` | `green`, `blue`, `test` | Deployment group the operator patches |
| `kind` | `env` | Assignment type; only environment variables are currently supported |
| `name` | string | Environment variable name |
| `value` | string | Environment variable value |
| `containerName` | optional string | Patch only this container when set; otherwise patch every container in the target Deployment |

### `POST /drain`

Called when a promotion or rollback decision has been made, before terminal cleanup.

Purpose:

- stop accepting new work on temporary paths
- return assignments that move the surviving deployment back toward direct production wiring
- give the plugin time to let temporary queues, streams, or proxies drain

The response shape is the same as `/prepare`.

### `GET /drain-status`

Called repeatedly while the `BlueGreenDeployment` is in `Draining`.

Response shape:

```json
{
  "drained": true,
  "message": "temporary input queues are empty and no consumers remain attached"
}
```

If a plugin does not expose `drainStatusPath`, the operator treats that inception point as drain-complete. If the configured maximum drain wait elapses first, the operator records `TimedOutMaybeSuccessful` and proceeds with cleanup.

### `POST /cleanup`

Called by the operator after `Completed` or `RolledBack`.

Purpose:

- delete transport-specific derived resources
- allow the operator to restore direct application wiring cleanly
- leave the plugin safe to idle or terminate

## Data Verification Contract

The plugin creates and notifies `testCase`s, but the final green/not-green decision still comes from the test container verify endpoint.

### Expected verify response

The operator expects JSON shaped like:

```json
{
  "passed": true,
  "testId": "order-17",
  "errorMessage": null
}
```

or on failure:

```json
{
  "passed": false,
  "testId": "order-17",
  "errorMessage": "downstream validation failed"
}
```

If the test is still in progress:

```json
{
  "passed": null,
  "testId": "order-17",
  "status": "observing",
  "errorMessage": null
}
```

## Observer Notification Body

`observer.notifyPath` is a single callback endpoint. Plugins include route metadata in the JSON body so the test container can distinguish mirrored traffic from weighted splitter traffic without requiring separate URLs. Route metadata is plugin-owned infrastructure metadata; applications do not need to add route fields to their business payloads.

Notification shape:

```json
{
  "testId": "order-17",
  "inceptionPoint": "incoming-orders",
  "route": "blue",
  "payload": {}
}
```

`route` values:

| Value | Meaning |
|---|---|
| `blue` | The resource was routed only to the candidate blue path. |
| `green` | The resource was routed only to the current green path. |
| `both` | The resource was duplicated to both green and blue. This is the queue `duplicator` behavior. |
| `unknown` | The plugin cannot determine the route for this observation. |

For RabbitMQ and Azure Service Bus, input routes come from the duplicator/splitter decision. Output routes come from the combiner source queue: `blueOutputQueue` maps to `blue`, and `greenOutputQueue` maps to `green`.

Queue plugin operator registration semantics:

- `blue`, `both`, and `unknown` observations register an operator `testCase`.
- `green` observations still call `observer.notifyPath`, but do not register an operator `testCase`; this prevents progressive splitter traffic sent only to green from becoming pending blue verification cases.

HTTP operator registration follows the same route semantics. Splitter/proxy
requests routed to blue register operator `testCase`s; green-only progressive
traffic can still be observed by the test container without creating pending
blue verification cases.

## Communication Diagrams

### 1. Runtime Discovery

```mermaid
flowchart LR
    OP["Operator Pod"]
    POD["Plugin Pod"]
    TEST["Test Container Pod"]

    OP -->|"injects env vars"| POD
    OP -->|"creates service"| TEST

    POD -->|"FLUIDBG_OPERATOR_URL"| OP
    POD -->|"FLUIDBG_TEST_CONTAINER_URL"| TEST

    note1["Discovery sources:<br/>operator service DNS<br/>test service DNS<br/>BGD name<br/>inception point name<br/>verify path template"] --> POD
```

### 2. Queue-Driven Rollout with Duplicator + Observer + Combiner

```mermaid
sequenceDiagram
    participant Src as Source Producer
    participant MQ as RabbitMQ
    participant In as incoming-orders plugin
    participant G as current green app
    participant B as candidate blue app
    participant Out as outgoing-results plugin
    participant T as test-container
    participant O as operator

    Src->>MQ: publish orders
    In->>MQ: consume inputQueue
    In->>MQ: publish greenInputQueue
    In->>MQ: publish blueInputQueue
    In->>O: POST /testcases
    In->>T: POST observer.notifyPath

    G->>MQ: consume greenInputQueue
    B->>MQ: consume blueInputQueue
    G->>MQ: publish greenOutputQueue
    B->>MQ: publish blueOutputQueue

    Out->>MQ: consume greenOutputQueue
    Out->>MQ: consume blueOutputQueue
    Out->>MQ: publish outputQueue
    Out->>O: POST /testcases
    Out->>T: POST observer.notifyPath

    O->>T: GET dataVerification.verifyPath
    T-->>O: {passed,errorMessage}
    O->>O: update testCase counts
    O->>O: decide promote or rollback
```

### 3. Prepare and Cleanup

```mermaid
sequenceDiagram
    participant O as operator
    participant P as plugin
    participant G as green deployment
    participant B as blue deployment
    participant T as test deployment

    O->>P: POST /prepare
    P-->>O: assignments[]
    O->>G: patch env vars
    O->>B: patch env vars
    O->>T: patch env vars
    O->>O: wait for rollout completion

    Note over O,P: observation and promotion happen here

    O->>P: POST /cleanup
    P-->>O: cleanup complete
    O->>O: remove plugin and test resources
```

### 4. Information Sources by Container

```mermaid
flowchart TD
    BGD["BlueGreenDeployment"]
    PLUGINCR["InceptionPlugin CR"]
    OP["Operator"]
    PPOD["Plugin Pod"]
    APP["Green/Blue App Pods"]
    TEST["Test Container Pod"]

    BGD -->|"inceptionPoints config"| OP
    PLUGINCR -->|"supportedRoles, lifecycle, topology"| OP
    OP -->|"prepare assignments"| APP
    OP -->|"test env + service"| TEST
    OP -->|"runtime env injection"| PPOD
    TEST -->|"verify response"| OP
    PPOD -->|"register testCase + notify test"| OP
    PPOD -->|"notifyPath call"| TEST
```

## Current Built-In RabbitMQ Plugin Behavior

```mermaid
flowchart TD
    SRC["Base input queue"]
    DUP["duplicator role"]
    SPLIT["splitter role"]
    GREENQ["green input queue"]
    BLUEQ["blue input queue"]
    GREEN["green app"]
    BLUE["blue app"]
    GOUT["green output queue"]
    BOUT["blue output queue"]
    COMB["combiner role"]
    OUT["base output queue"]
    TEST["test container"]
    OP["operator"]

    SRC --> DUP
    SRC --> SPLIT
    DUP -->|"route both"| GREENQ
    DUP -->|"route both"| BLUEQ
    SPLIT -->|"route green"| GREENQ
    SPLIT -->|"route blue"| BLUEQ
    GREENQ --> GREEN --> GOUT
    BLUEQ --> BLUE --> BOUT
    GOUT --> COMB
    BOUT --> COMB
    COMB --> OUT
    DUP -->|"register and notify"| OP
    DUP --> TEST
    COMB -->|"notify source route"| TEST
```

### Duplicator

- consumes `duplicator.inputQueue`
- republishes each matching message to:
  - `duplicator.greenInputQueue`
  - `duplicator.blueInputQueue`
- returns env assignments for:
  - `duplicator.greenInputQueueEnvVar`
  - `duplicator.blueInputQueueEnvVar`

### Splitter

- consumes `splitter.inputQueue`
- routes messages to green or blue based on the current traffic percentage
- initializes that percentage from `FLUIDBG_TRAFFIC_PERCENT` when the plugin starts
- updates the percentage at runtime when the operator calls lifecycle `trafficShiftPath` with `{ "trafficPercent": n }`
- does not require the operator to patch env vars or restart the plugin pod during normal progressive shifting

### Combiner

- consumes:
  - `combiner.greenOutputQueue`
  - `combiner.blueOutputQueue`
- publishes to:
  - `combiner.outputQueue`
- returns env assignments for:
  - `combiner.greenOutputQueueEnvVar`
  - `combiner.blueOutputQueueEnvVar`

### Observer

- filters messages according to `observer.match`
- extracts `testId`
- calls `observer.notifyPath` with `route` metadata in the body
- registers the `testCase` with the operator for `blue`, `both`, and `unknown` routes
- does not register green-only observations as operator verification cases

### Promotion and Rollback Safety

RabbitMQ loss prevention is handled by the plugin state machine plus the
operator drain phase:

- Duplicator and splitter queue consumers acknowledge the source message only after the plugin has successfully published the required downstream copy or route.
- During drain, the plugin stops accepting new temporary work and reports `drained: true` only when temporary paths are empty or no longer have active work that must be moved.
- For rollback, blue-only temporary messages are either consumed by blue during the drain window or left/moved so the base queue can continue safely after cleanup.
- For promotion, surviving traffic is moved back to the base wiring before cleanup removes temporary resources.
- If drain exceeds the configured wait, the operator records `TimedOutMaybeSuccessful` and proceeds; this is explicit risk accounting rather than silent success.

## Current Built-In Azure Service Bus Plugin Behavior

The Azure Service Bus plugin is named `azure-servicebus` in the built-in
`InceptionPlugin` registration and ships as
`ghcr.io/dlahmad/fbg-plugin-azure-servicebus`. It supports the same queue roles
as RabbitMQ where Service Bus semantics map cleanly: `duplicator`, `splitter`,
`combiner`, `observer`, `writer`, and `consumer`.

```mermaid
flowchart TD
    SRC["Base Service Bus queue"]
    ASB["fbg-plugin-azure-servicebus"]
    GREENQ["green input queue"]
    BLUEQ["blue input queue"]
    GREEN["green app"]
    BLUE["blue app"]
    GOUT["green output queue"]
    BOUT["blue output queue"]
    OUT["base output queue"]
    TEST["test container"]
    OP["operator"]

    SRC -->|"peek-lock"| ASB
    ASB -->|"duplicate or split"| GREENQ
    ASB -->|"duplicate or split"| BLUEQ
    GREENQ --> GREEN --> GOUT
    BLUEQ --> BLUE --> BOUT
    GOUT --> ASB
    BOUT --> ASB
    ASB --> OUT
    ASB -->|"complete after publish"| SRC
    ASB -->|"register and notify"| OP
    ASB --> TEST
```

### Authentication to Azure

The plugin supports two Azure authentication modes:

| Mode | Config | Use |
|---|---|---|
| `connectionString` | `connectionString` with `Endpoint`, `SharedAccessKeyName`, and `SharedAccessKey` | Uses Service Bus SAS tokens for runtime send, receive, complete, unlock, and data-plane queue management. |
| `workloadIdentity` | `fullyQualifiedNamespace` plus AKS workload identity env vars or explicit `auth.tenantId`, `auth.clientId`, `auth.federatedTokenFile` | Exchanges the projected Kubernetes service account token for Microsoft Entra tokens and uses Bearer auth for Service Bus runtime access. |

For AKS workload identity, the plugin expects the standard projected values
`AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, and `AZURE_FEDERATED_TOKEN_FILE` unless
they are set explicitly in plugin config. The plugin requests Service Bus data
plane tokens for `https://servicebus.azure.net/.default`. If
`management.subscriptionId` and `management.resourceGroup` are also set, queue
create/delete/status calls use Azure Resource Manager with
`https://management.azure.com/.default`.

The Kubernetes service account used by the plugin pod must be configured for
Azure Workload Identity by the cluster owner, including the
`azure.workload.identity/use: "true"` pod label or equivalent chart overlay, and
the managed identity must have Service Bus data-plane permissions. Automatic
queue create/delete additionally requires permission to manage
`Microsoft.ServiceBus/namespaces/queues/*` in the target namespace resource.

`InceptionPlugin.spec.container` supports pod-level metadata needed for this:
`podLabels`, `podAnnotations`, and `serviceAccountName`. The Helm chart exposes
these for the built-in Azure Service Bus plugin under
`builtinPlugins.azureServiceBus.workloadIdentity`.

### Service Bus Semantics

- The plugin uses Service Bus REST runtime APIs directly to keep the image small.
- Reads use peek-lock, not receive-and-delete.
- The source message is completed only after the downstream send and optional observer notification have succeeded.
- On processing failure, the plugin unlocks the message so Service Bus can redeliver it.
- The writer role publishes JSON payloads to `writer.targetQueue` and maps `properties` to Service Bus custom message headers.
- Filters and test-id selectors support both `queue.*` and `servicebus.*` field namespaces. Applications do not need to add FluidBG route fields to message bodies.

### Promotion and Rollback Safety

Azure Service Bus does not expose RabbitMQ-style active consumer counts through
the portable REST path used by this plugin. The plugin therefore uses the
following safety model:

- During drain, duplicator and splitter roles stop taking new base-queue work and move available messages from temporary green/blue queues back to the base queue.
- Combiner roles stop forwarding temporary output work during drain and rely on queue counts before cleanup.
- Drain status reports success only after temporary queues are empty for a configurable stability window, `drainStabilitySeconds` defaulting to `65`.
- The stability window covers normal one-minute Service Bus locks so messages locked by old consumers can either complete or become visible again before cleanup.
- If the configured operator drain timeout is shorter than this window, the operator may still time out and record explicit risk through `TimedOutMaybeSuccessful`.

## Current Built-In HTTP Plugin Behavior

The HTTP plugin is one combined standalone service with `splitter`, `observer`,
`mock`, and `writer` roles.

```mermaid
flowchart LR
    CLIENT["caller"]
    HP["fbg-plugin-http"]
    GREEN["green endpoint"]
    BLUE["blue endpoint"]
    UP["real upstream"]
    TEST["test container"]
    OP["operator"]

    CLIENT --> HP
    HP -->|"progressive route green"| GREEN
    HP -->|"progressive route blue"| BLUE
    BLUE -->|"egress call via plugin"| HP
    HP -->|"proxy fallback"| UP
    HP -->|"observer or mock callback"| TEST
    HP -->|"register blue case"| OP
    TEST -->|"POST /write"| HP
    HP -->|"write to blue target"| BLUE
```

### Splitter / Proxy

- proxies requests through the plugin service
- routes traffic to `greenEndpoint` or `blueEndpoint` from config using the current traffic percentage
- reports route metadata as `green`, `blue`, or `unknown`
- updates progressive percentage through lifecycle `trafficShiftPath`

### Observer / Mock

- filters requests according to `match`/`filters`
- extracts `testId`
- posts observer notifications with route metadata in the JSON body
- can return mock responses from the test container when configured as a mock

### Writer

- exposes `/write`
- forwards test-container initiated HTTP calls to `targetUrl` or the configured blue endpoint
- is reached through the env var declared by `writeEnvVar`

### Promotion and Rollback Safety

HTTP cannot provide queue-style durable replay for in-flight requests. The
operator and plugin minimize request loss by ordering traffic restoration before
resource removal:

- Progressive changes are applied through `POST /traffic`, so the plugin pod is not restarted when percentages change.
- Drain tells the plugin to stop accepting new temporary routing decisions before cleanup begins.
- The operator applies restore assignments so application env vars point back to direct green/blue endpoints before deleting the plugin service.
- Existing calls already accepted by the HTTP plugin are allowed to complete during the drain window.
- Cleanup removes the plugin only after drain succeeds or the configured timeout is reached.
- If the timeout is hit, the status records that drain was not proven fully successful; the operator does not pretend zero-loss was guaranteed.

## Practical Notes

- Standalone plugins and the test container are temporary rollout resources.
- Service discovery must be namespace-qualified when the caller is outside the test namespace.
- `testCasesObserved` only counts finalized cases.
- `testCasesPending` must be included when you want to know whether the operator has started tracking traffic already.
