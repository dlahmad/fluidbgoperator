# FluidBG Operator Architecture

## Overview

FluidBG is a Kubernetes operator for blue-green and progressive delivery of queue
and HTTP workloads. A candidate deployment is exposed to controlled live traffic,
verifier containers decide whether observed test cases passed, and the operator
promotes or rolls back from configured success criteria.

The operator is intentionally transport-agnostic. Transport behavior lives in
`InceptionPlugin` resources and their plugin containers. A
`BlueGreenDeployment` selects plugins through named `inceptionPoints`, activates
one or more roles, and provides plugin-specific config. The operator renders the
declared Kubernetes resources, calls lifecycle endpoints, tracks test cases, and
removes temporary resources after promotion or rollback.

```mermaid
flowchart TD
    SRC["Production source<br/>Queue or HTTP client"]
    SPLIT["Splitter / duplicator plugin"]
    GREEN["Current green deployment"]
    BLUE["Candidate blue deployment"]
    OBS["Observer / combiner / mock plugin"]
    TEST["Verifier test container"]
    OP["FluidBG operator"]
    OUT["Production output / upstream"]

    SRC --> SPLIT
    SPLIT -->|"green route"| GREEN
    SPLIT -->|"blue route"| BLUE
    SPLIT -->|"register test case"| OP
    SPLIT -->|"notify observation"| TEST
    GREEN --> OBS
    BLUE --> OBS
    OBS -->|"route metadata + payload"| TEST
    OBS --> OUT
    OP -->|"GET verifyPath"| TEST
    TEST -->|"passed / failed / pending"| OP
    OP -->|"promote, rollback, drain, cleanup"| SPLIT
    OP -->|"promote, rollback, drain, cleanup"| OBS
```

## Core Concepts

| Term | Current Meaning |
|---|---|
| `BlueGreenDeployment` | Names the green deployment, candidate blue template, inception points, verifier containers, and promotion strategy. |
| `InceptionPoint` | A named traffic interception point. It references one plugin, activates `roles`, supplies arbitrary plugin `config`, and can define drain options/resources. |
| `InceptionPlugin` | A namespaced plugin registration CRD. It declares image, topology, supported roles, lifecycle paths, field namespaces, config schema, injected env vars, and optional features. |
| Plugin role | The behavior activated for an inception point: `duplicator`, `splitter`, `combiner`, `observer`, `mock`, `writer`, or `consumer`. |
| Verifier test container | User-owned HTTP service that stores domain observations and returns `passed: true`, `passed: false`, or `passed: null` for a `testId`. |
| State store | Operator persistence for registered test cases and counts. Implemented backends are `memory` and `postgres`. |
| Progressive shifting | Weighted blue traffic controlled by the operator through plugin lifecycle `trafficShiftPath` calls. |

Older docs described `mode`, `direction`, and transport-specific orchestration
kinds. The current CRD model is role-based. `FLUIDBG_MODE` remains only as a
backward-compatible SDK fallback; the operator injects `FLUIDBG_ACTIVE_ROLES`.

## CRD Model

All Kubernetes resources use API group `fluidbg.io/v1alpha1`.

### `InceptionPlugin`

An `InceptionPlugin` declares the reusable plugin contract:

```yaml
apiVersion: fluidbg.io/v1alpha1
kind: InceptionPlugin
metadata:
  name: http
spec:
  description: "HTTP transport plugin for observing, mocking, and writing HTTP traffic"
  image: ghcr.io/dlahmad/fbg-plugin-http:0.1.0
  supportedRoles: [splitter, observer, mock, writer]
  topology: standalone
  lifecycle:
    preparePath: /prepare
    drainPath: /drain
    drainStatusPath: /drain-status
    cleanupPath: /cleanup
    trafficShiftPath: /traffic
  features:
    supportsProgressiveShifting: true
  fieldNamespaces: [http]
  configSchema:
    type: object
  container:
    ports:
      - name: http
        containerPort: 9090
```

Important fields:

| Field | Meaning |
|---|---|
| `supportedRoles` | Roles this plugin can run for an inception point. |
| `topology` | `standalone`, `sidecar-blue`, or `sidecar-test`. Built-in plugins currently run as `standalone`. |
| `fieldNamespaces` | Filter/selector namespaces supported by the plugin, for example `http` or `queue`. |
| `configSchema` | JSON Schema used by the operator to validate the inception point config. |
| `lifecycle` | HTTP endpoints called by the operator for prepare, drain, cleanup, and traffic shifting. |
| `injects` | Env vars the operator patches into green, blue, or test containers from plugin config/template values. |
| `features.supportsProgressiveShifting` | Required for progressive strategies that use standalone splitter plugins. |

### `BlueGreenDeployment`

The current `inceptionPoints` shape is:

```yaml
inceptionPoints:
  - name: incoming-orders
    pluginRef:
      name: rabbitmq
    roles: [duplicator, observer]
    drain:
      maxWaitSeconds: 60
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

Promotion supports data-verification and custom-verification criteria:

```yaml
promotion:
  data:
    minTestCases: 100
    successRate: 0.98
    timeoutSeconds: 1800
  strategy:
    type: progressive
    progressive:
      steps:
        - { trafficPercent: 5, observeCases: 20, successRate: 0.99 }
        - { trafficPercent: 25, observeCases: 50, successRate: 0.98 }
        - { trafficPercent: 100, observeCases: 50, successRate: 0.98 }
      rollbackOnStepFailure: true
      stepTimeoutMinutes: 15
```

### `StateStore`

```yaml
apiVersion: fluidbg.io/v1alpha1
kind: StateStore
metadata:
  name: default
spec:
  type: postgres
  postgres:
    url: "postgres://fluidbg:secret@postgres.fluidbg:5432/fluidbg"
    tableName: fluidbg_cases
    ttlSeconds: 86400
```

`memory` is useful for development and tests. `postgres` is the production
backend. Redis is not implemented.

## Operator Reconciliation

For one `BlueGreenDeployment`, reconciliation does the following:

1. Validate tests, promotion settings, plugin references, supported roles,
   supported field namespaces, plugin config schemas, and progressive capability.
2. Render verifier test deployments/services.
3. Render plugin ConfigMaps, Deployments, and Services from `InceptionPlugin`.
4. Inject runtime env vars into plugin containers.
5. Call plugin `preparePath` and patch returned assignments into green, blue,
   and test deployments.
6. Register and poll test cases through the operator HTTP API and verifier
   `verifyPath`.
7. Apply progressive traffic updates by calling plugin `trafficShiftPath`.
8. On promotion or rollback, enter draining, call plugin `drainPath`, poll
   `drainStatusPath`, then call `cleanupPath`.
9. Remove temporary test/plugin resources and restore direct assignment values
   where plugins declared restore templates.

The operator also prevents two rollouts of the same `BlueGreenDeployment` from
colliding while the previous rollout's temporary inception resources still
exist. Different `BlueGreenDeployment` names can run concurrently because
generated resource names include the BGD name and inception point.

## Plugin Runtime Contract

Standalone plugin containers receive these operator-managed env vars:

| Env Var | Meaning |
|---|---|
| `FLUIDBG_OPERATOR_URL` | Base URL of the operator API. |
| `FLUIDBG_TESTCASE_REGISTRATION_URL` | Full operator URL for registering test cases. |
| `FLUIDBG_TEST_CONTAINER_URL` | Base URL of the selected verifier test service. |
| `FLUIDBG_TESTCASE_VERIFY_PATH_TEMPLATE` | Verifier path template, for example `/result/{testId}`. |
| `FLUIDBG_INCEPTION_POINT` | Active inception point name. |
| `FLUIDBG_BLUE_GREEN_REF` | Owning `BlueGreenDeployment` name. |
| `FLUIDBG_ACTIVE_ROLES` | Comma-separated roles activated for this plugin instance. |
| `FLUIDBG_CONFIG_PATH` | Mounted plugin config file path, currently `/etc/fluidbg/config.yaml`. |

Plugins should import shared Rust types from `sdk/rust` or generate clients from
`sdk/spec/plugin-api-v1alpha1.openapi.yaml`. The SDK defines lifecycle payloads,
assignments, test-case registration, observer notifications, traffic routes, and
traffic shifting.

## Lifecycle Endpoints

The operator calls lifecycle endpoints declared by the plugin CRD:

| Endpoint | Called When | Expected Behavior |
|---|---|---|
| `POST /prepare` | Before observation starts | Create derived transport resources and return assignments to patch into green/blue/test containers. |
| `POST /drain` | After promotion or rollback decision | Stop accepting new temporary work and return assignments that move the surviving deployment toward direct production wiring. |
| `GET /drain-status` | While phase is `Draining` | Return whether temporary resources are drained. Missing endpoint means drain-complete. |
| `POST /cleanup` | After drain completion or timeout | Delete derived transport resources and leave plugin safe to terminate. |
| `POST /traffic` | Initial rollout and progressive step changes | Update blue traffic percentage without restarting the plugin pod. |

Traffic shifting uses this SDK payload:

```json
{ "trafficPercent": 25 }
```

Built-in plugins keep the current percentage in process memory and update it via
`POST /traffic`. `FLUIDBG_TRAFFIC_PERCENT` is only the startup default; normal
progressive operation does not patch env vars or restart plugin pods.

## Progressive Shifting

Progressive strategy is allowed only when a referenced standalone splitter plugin
advertises `features.supportsProgressiveShifting: true`. The operator enforces
that requirement before rollout.

```mermaid
stateDiagram-v2
    [*] --> Pending
    Pending --> Observing: resources prepared and initial traffic set
    Observing --> Observing: step passed, POST /traffic with next percentage
    Observing --> Draining: final step passed or rollback needed
    Draining --> Completed: promoted and cleanup done
    Draining --> RolledBack: rollback cleanup done
```

Traffic route semantics are plugin-owned:

| Route | Meaning |
|---|---|
| `blue` | Resource was routed only to candidate blue. |
| `green` | Resource was routed only to current green. |
| `both` | Resource was duplicated to green and blue. |
| `unknown` | Plugin cannot determine the route. |

RabbitMQ and HTTP both support progressive splitting where it makes transport
sense. RabbitMQ uses stable hashing to decide whether a message goes to blue at
the current percentage. HTTP uses the same route decision for proxy traffic.

## Built-In Plugins

| Plugin | Roles | Topology | Progressive | Notes |
|---|---|---|---|---|
| `http` | `splitter`, `observer`, `mock`, `writer` | `standalone` | yes | One combined HTTP service exposes proxy/splitter, observer/mock behavior, and `/write`. |
| `rabbitmq` | `duplicator`, `splitter`, `observer`, `writer`, `consumer`, `combiner` | `standalone` | yes | Handles queue fanout, weighted split, observer notifications, writes, consumption, and output combining. |

Built-in plugin manifests live in `builtin-plugins/` and are also templated by
the Helm chart for each configured namespace.

## Filtering, Selectors, and Field Namespaces

Plugins declare supported field namespaces. The built-in HTTP plugin supports
`http`; the RabbitMQ plugin supports `queue`.

Examples:

```yaml
match:
  - field: http.method
    equals: POST
  - field: http.path
    matches: "^/orders$"
  - field: http.body
    jsonPath: $.type
    matches: "^order$"
```

```yaml
testId:
  field: queue.body
  jsonPath: $.orderId
```

Exactly one of `equals` or `matches` is valid for a match condition. Selectors
can use a field plus optional `jsonPath`/`pathSegment`, or a static `value`.

## Test Container Contract

Verifier containers hold observation state. The operator does not infer
application correctness from message bodies or HTTP payloads. Plugins notify the
verifier, register test cases when appropriate, and the operator polls the
verifier result path.

Verifier response:

```json
{ "testId": "order-17", "passed": true, "errorMessage": null }
```

Pending response:

```json
{ "testId": "order-17", "passed": null, "status": "observing" }
```

Observer notifications include infrastructure route metadata in the body:

```json
{
  "testId": "order-17",
  "inceptionPoint": "incoming-orders",
  "route": "both",
  "payload": {}
}
```

For RabbitMQ, `both` reflects duplicator fanout to blue and green. For combiner
observations, the route is derived from the source queue, not from application
payload content.

## Project Structure

```text
operator/              Rust operator crate, CRDs, controller, state stores, HTTP API
plugins/http/          Combined HTTP splitter/observer/mock/writer plugin
plugins/rabbitmq/      Combined RabbitMQ duplicator/splitter/combiner/writer/observer plugin
sdk/                   Versioned Rust SDK and language-neutral OpenAPI specs
crds/                  Generated CRD manifests
builtin-plugins/       Built-in InceptionPlugin manifests
charts/                Helm chart with CRDs and built-in plugin CR templates
deploy/                Raw operator deployment and RBAC
e2e/                   Kind-based end-to-end suite
testenv/               RabbitMQ, Postgres, and kind support manifests
```

Important implementation boundaries:

| Area | Responsibility |
|---|---|
| `operator/src/controller.rs` | Reconcile phase machine. |
| `operator/src/controller/plugin_lifecycle.rs` | Plugin lifecycle HTTP client. |
| `operator/src/controller/promotion.rs` | Promotion validation and decision logic. |
| `operator/src/plugins/reconciler.rs` | Plugin-agnostic Kubernetes resource rendering. |
| `sdk/rust` | Shared model and helper types consumed by built-in plugins. |

## Safety Properties

- Green keeps serving unless the configured promotion path replaces it.
- Temporary plugin and test resources are removed after promotion or rollback.
- Draining gives plugins a chance to move or consume temporary work before
  cleanup; drain timeout is explicit and reflected in status.
- Progressive shifting changes plugin state through lifecycle HTTP calls, so it
  does not require plugin pod restarts.
- The test container owns semantic correctness; application payload formats are
  not part of the operator contract.
- `postgres` state store preserves registered cases across operator restarts.
- CRD models and plugin API models are versioned under `v1alpha1`.
