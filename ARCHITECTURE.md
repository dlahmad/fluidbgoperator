# FluidBG Operator — Architecture

## Overview

FluidBG is a Kubernetes operator that automates **blue-green deployments for message-queue and HTTP consumers**. It validates a new version of a service ("blue") against live traffic before promoting it to production ("green") — or rolling it back — based on configurable acceptance criteria.

The core idea: wire **inception points** around the blue deployment so that traffic is observed, mocked, split, combined, or injected as needed. The operator does not hardcode how a given transport works. Each `InceptionPlugin` declares an orchestration contract, such as `queue-split`, `queue-combine`, or `http-split`; the `BlueGreenDeployment` supplies concrete queue names, service names, env vars, and deployment specs; the operator executes the declared plugin contract. Plugins forward intercepted data to a **test container** that decides, per test run, whether blue behaved correctly. The operator tracks test runs, counts green/not-green verdicts, drives promotion, and restores direct wiring after the decision.

---

## Core Concepts

| Term | Definition |
|---|---|
| **Inception Point** | A named place in the traffic flow where FluidBG intercepts data. Has a **mode**, one or more **directions**, and references a plugin. |
| **Mode** | What the inception point does: `trigger`, `passthrough-duplicate`, `reroute-mock`, or `write`. |
| **Direction** | `ingress` (traffic into blue), `egress` (traffic out of blue), or both (same plugin handles both flows). |
| **Inception Plugin** | A pluggable, CRD-registered container that implements interception. Plugins are **external containers** — any language, any registry. |
| **Test Container** | User-supplied HTTP server that holds observation state and answers one question per test run: **green or not green**. |
| **Test ID** | User-defined correlation key extracted from traffic by a selector. All observations for a test run are grouped by `testId`. |
| **State Store** | Pluggable persistence for the operator's tracking state (active testIds, verdicts, counts). Backend types: `memory`, `redis`, `postgres`. |
| **Plugin Orchestration Contract** | The plugin-declared set of lifecycle actions the operator must perform: deploy plugin resources, patch application env vars, create or reference transport resources, and clean up or restore after promotion/rollback. |
| **Split Plugin** | A plugin that takes one production input and fans it out to green and blue paths. For queues this may mean original queue → green queue + blue queue; for HTTP it may mean proxying one request stream to two backends. |
| **Combine Plugin** | A plugin that merges blue and green output paths into one result path using plugin-defined rules. For queues this may mean blue output queue + green output queue → production result queue. |

### The Operator's Logic Is Minimal

1. **Register** a test run each time a trigger fires (with a user-defined timeout).
2. **Track** runs by `testId` in the configured state store.
3. **Poll** the test container for a verdict on each run.
4. **Time out** runs that don't get a verdict in time — these count as not-green.
5. **Count** green vs not-green verdicts.
6. **Decide** promotion based on the user-defined success-rate threshold.
7. **Execute terminal orchestration** defined by the plugin contract: after success, promoted blue is restored to direct production wiring; after failure, green is restored; temporary test/plugin resources are removed in both cases.

The operator does **not** hold observation state. The test container owns all domain knowledge. The operator is a bookkeeper that wires traffic to the test container and counts answers.

---

## High-Level Flow

```mermaid
flowchart TD
    SRC["Source<br/>(Queue / HTTP Client)"]
    SRC -->|"split plugin<br/>transport-specific"| SPLIT["Split Inception"]
    SPLIT -->|"register testId"| OP["Operator"]
    SPLIT -->|"notify trigger"| TC["Test Container"]
    SPLIT -->|"green path"| GREEN["Green<br/>(live)"]
    SPLIT -->|"blue path"| BLUE["Blue<br/>(candidate)"]

    BLUE -->|"egress traffic"| OUT["Output Inception<br/>observe / mock / combine"]
    GREEN -->|"egress traffic"| OUT
    OUT -->|"observations"| TC
    OUT -->|"combined result or passthrough"| REAL["Real Output / Upstream"]

    TC -->|"write plugin<br/>inject traffic"| BLUE

    OP -->|"GET /result/{testId}"| TC
    TC -->|"passed: true/false"| OP

    OP -->|"successRate met"| PROM["Promote / Rollback"]
```

---

## Capabilities

Plugins are described by two separate ideas:

- **Traffic capability**: what the plugin does to traffic.
- **Orchestration contract**: what Kubernetes and transport setup the operator must perform for that plugin.

### Traffic Capabilities

| Capability | Purpose | Typical Stage | Creates Test Runs? |
|---|---|---|---|
| `split` | Take one production stream and fan it out to green and blue paths | input | Usually |
| `combine` | Merge blue and green output paths into one result path | output | No |
| `sink` | Consume a resource branch and intentionally terminate it | input or output | No |
| `trigger` | Register a test run and notify the test container for matched traffic | input or output | Yes |
| `passthrough-duplicate` | Forward traffic normally and send a copy to the test container | input or output | Optional |
| `reroute-mock` | Replace an external dependency with the test container's response | output | No |
| `write` | Expose a writer endpoint the test container can call to inject traffic | input or output | No |

For backward compatibility, `mode` still names these capabilities in the CRD. New plugin definitions should prefer the simpler capability vocabulary: `split`, `combine`, `sink`, `trigger`, `mock`, `write`, and `observe`. Older names such as `passthrough-duplicate` and `reroute-mock` map to `observe` and `mock`.

```mermaid
flowchart LR
    subgraph S["split"]
        S1["production input"] --> S2["Plugin"]
        S2 --> S3["green path"]
        S2 --> S4["blue path"]
        S2 -->|register / notify| S5["Operator + Test Container"]
    end
```

```mermaid
flowchart LR
    subgraph C["combine"]
        C1["green output"] --> C3["Plugin"]
        C2["blue output"] --> C3
        C3 --> C4["single result output"]
    end
```

```mermaid
flowchart LR
    subgraph RM["reroute-mock"]
        RM1["Blue"] --> RM2["Plugin"]
        RM2 -->|route + return mock| RM3["Test Container"]
    end
```

```mermaid
flowchart LR
    subgraph W["write"]
        W1["Test Container"] -->|POST /write| W2["Plugin"]
        W2 -->|inject| W3["Blue's queue / HTTP ingress"]
    end
```

### Stages

The old `direction` field is best understood as the stage where the plugin attaches:

| Stage | What the plugin sees |
|---|---|
| `ingress` / input | Traffic entering the application: messages consumed from input queues, HTTP requests to the service |
| `egress` / output | Traffic leaving the application: output queues, outbound HTTP calls, emitted events |
| both | A plugin that manages both sides, usually with separate config blocks |

### Capability × Stage Validity

| Capability | input | output | both |
|---|---|---|---|
| `split` | ✓ | possible, but uncommon | plugin-defined |
| `combine` | uncommon | ✓ | plugin-defined |
| `sink` | ✓ | ✓ | plugin-defined |
| `trigger` | ✓ | ✓ | ✓ |
| `passthrough-duplicate` | ✓ | ✓ | ✓ |
| `reroute-mock` | ✗ (would replace blue itself) | ✓ | ✗ |
| `write` | ✓ | ✓ | ✓ |

The operator rejects invalid combinations at CRD-validation time.

### Orchestration Contracts

The plugin system owns transport-specific lifecycle behavior. The operator only executes the contract declared by the plugin:

| Orchestration Kind | Operator Actions | Plugin Responsibility |
|---|---|---|
| `queue-split` | Patch green and blue Deployment env vars to temporary queues; deploy the split plugin; restore direct queue wiring after promotion/rollback; clean up plugin/test resources | Consume from the original queue and publish to the green and blue queues without acknowledging the source message until fanout succeeds |
| `queue-combine` | Patch output env vars to temporary blue/green output queues; deploy the combine plugin; restore direct output wiring after terminal state | Consume/merge from blue and green output queues and publish to the result queue according to plugin rules |
| `http-split` | Patch service/env routing to the HTTP split plugin; deploy proxy resources; restore direct service routing after terminal state | Proxy or mirror HTTP traffic according to plugin-specific semantics |

This distinction matters: duplicating HTTP requests is not the same operation as creating two queues, and combining queue outputs is not the same as merging HTTP responses. The `InceptionPlugin` declares the orchestration kind; the `BlueGreenDeployment` supplies concrete names and values; the operator reconciles Kubernetes state from those declarations.

### Plugin-Advertised Capabilities

Progressive shifting and event notifications are not configured ad hoc on an inception point. They are advertised by the plugin and then used by the BGD strategy layer:

| Advertised Capability | Meaning |
|---|---|
| `supportsProgressiveShifting` | The plugin can perform weighted traffic shifting. This is a splitter capability only. Combiners do not own progressive shifting. |
| `canNotifyTestOnEvents` | The plugin can optionally notify the test container for every matching resource that passes through it. This may be used by splitters, combiners, or observers. |
| `providesSink` | The plugin can expose a sink path that consumes and terminates a resource branch. |

The BGD should only enable progressive strategy steps when the referenced splitter plugin advertises `supportsProgressiveShifting: true`.

---

## Field References, Filters, Selectors

All matching and extraction across every plugin type is built on a single abstraction: the **field reference**.

### Field Namespaces

| Field | Namespace | Description |
|---|---|---|
| `http.method` | `http` | HTTP method (`GET`, `POST`, etc.) |
| `http.path` | `http` | URL path (e.g. `/orders/123`) |
| `http.header.<name>` | `http` | Request header by name |
| `http.query.<name>` | `http` | Query parameter by name |
| `http.body` | `http` | Request body (parsed as JSON when `jsonPath` is used) |
| `queue.body` | `queue` | Message body (parsed as JSON when `jsonPath` is used) |
| `queue.property.<name>` | `queue` | Message property/attribute by name |

A plugin declares which namespaces it supports (`fieldNamespaces: [http]`, `[queue]`, etc.). The operator rejects filters/selectors using unsupported namespaces at CRD-validation time. New plugins can add their own namespaces (`grpc.*`, `amqp.*`, …) without changes to the filter engine.

### Filters

A filter is a condition on a field. All conditions in a filter's `match` list must pass (AND):

```yaml
match:
  - field: "http.method"
    equals: "POST"                       # exact string match
  - field: "http.path"
    matches: "^/orders$"                 # regex match
  - field: "http.body"
    jsonPath: "$.type"
    matches: "^order$"                   # regex on JSON-extracted value
```

| Property | Required | Notes |
|---|---|---|
| `field` | yes | A field reference |
| `equals` | no | Exact string match |
| `matches` | no | Regex match (RE2 syntax) |
| `jsonPath` | no | JSONPath into the field value (only valid for `*.body` fields) |

Exactly one of `equals` or `matches` must be provided.

### Selectors

A selector extracts a value — used for `testId` extraction.

```yaml
testId:
  field: "http.body"
  jsonPath: "$.orderId"

# OR: a header / query / path-segment / queue property
testId:
  field: "http.header.X-Test-Id"

testId:
  field: "http.path"
  pathSegment: 2                         # /orders/{seg2}/process

testId:
  field: "queue.property.correlationId"

# OR: static value (all traffic grouped under one test run)
testId:
  value: "fixed-id"
```

| Property | Required | Notes |
|---|---|---|
| `field` | yes* | Field reference (*unless `value` is set) |
| `jsonPath` | no | For `*.body` fields |
| `pathSegment` | no | 1-indexed segment; only for `http.path` |
| `value` | no* | Static value (*required if `field` is not set) |

If extraction fails, the traffic is still delivered to blue but no test case is created; the operator logs a warning.

---

## Plugin System

Plugins are the key extension point. The operator does **not** hardcode plugin types. A plugin is:

- An external container image (any language, any registry), plus
- An `InceptionPlugin` CRD that declares what it does, how it deploys, what it injects, and a JSON Schema for its user-facing config.

**No operator code changes are required to add a new plugin.** The operator treats the CRD as a declarative blueprint.

### Deployment Topologies

| Topology | Description | Typical Use |
|---|---|---|
| `sidecar-blue` | Plugin injected as a sidecar into the blue pod | HTTP / gRPC proxy — intercepts blue's in-process traffic |
| `sidecar-test` | Plugin injected as a sidecar into the test container pod | Rare; e.g., a co-located mock server |
| `standalone` | Plugin deployed as a separate Deployment + Service | Queue duplicators, write gateways — independent of blue's pod |

### Plugin Runtime Contract

Every plugin container receives these standard environment variables from the operator:

| Env Var | Description |
|---|---|
| `FLUIDBG_OPERATOR_URL` | URL of the operator's case-registration API |
| `FLUIDBG_TEST_CONTAINER_URL` | Base URL of the test container |
| `FLUIDBG_INCEPTION_POINT` | Name of the inception point this instance serves |
| `FLUIDBG_MODE` | `trigger` / `passthrough-duplicate` / `reroute-mock` / `write` |
| `FLUIDBG_DIRECTIONS` | Active directions: `ingress`, `egress`, or `ingress,egress` |
| `FLUIDBG_CONFIG_PATH` | Path to the mounted YAML config file |

A plugin must:

1. Read the YAML config at `FLUIDBG_CONFIG_PATH`.
2. For `trigger` mode: `POST` to `{FLUIDBG_OPERATOR_URL}/cases` with `{testId, inceptionPoint, triggeredAt}` when matched.
3. Notify the test container by `POST`ing to `{FLUIDBG_TEST_CONTAINER_URL}{notifyPath}` (paths substitute `{testId}`).
4. Forward / route / inject traffic per `FLUIDBG_MODE` and `FLUIDBG_DIRECTIONS`.

Plugins are **stateless**. They hold no observation state, no test results, no verdicts. All state lives in the test container or the operator's state store.

### How the Operator Deploys Plugins

When reconciling a `BlueGreenDeployment`, the operator processes each inception point:

1. **Look up** the referenced `InceptionPlugin` CRD.
2. **Validate** that the plugin supports the requested mode and direction, and that the user's `config` matches the plugin's `configSchema`.
3. **Generate** a ConfigMap from the user's `config` (optionally transformed via the plugin's `configTemplate`).
4. **Deploy** based on topology:
   - `sidecar-blue` / `sidecar-test`: inject the plugin container as a sidecar into the target pod, mount the ConfigMap.
   - `standalone`: create a `Deployment` + `Service` + ConfigMap.
5. **Inject** standard runtime env vars into the plugin container.
6. **Resolve** plugin-declared env-var injections into the blue and/or test containers:
   - For sidecars injecting into blue: value is `http://localhost:{port}` (same-pod networking).
   - For standalone plugins injecting into the test container: value is `http://{plugin-service}:{port}/write`.

Because every step is driven by the CRD, **adding a plugin means: publish a container image + apply an `InceptionPlugin` CRD.** No operator rebuild.

### Plugin Catalog (Built-in)

FluidBG ships with these plugins (each is an `InceptionPlugin` CRD + container image pre-registered by the installer):

| Plugin | Modes | Directions | Topology |
|---|---|---|---|
| `http-proxy` | `trigger`, `passthrough-duplicate`, `reroute-mock` | ingress, egress | sidecar-blue |
| `http-writer` | `write` | ingress, egress | standalone |
| `rabbitmq-duplicator` | `trigger`, `passthrough-duplicate` | ingress, egress | standalone |
| `rabbitmq-writer` | `write` | ingress, egress | standalone |
| `kafka-duplicator` | `trigger`, `passthrough-duplicate` | ingress, egress | standalone |
| `kafka-writer` | `write` | ingress, egress | standalone |

Additional plugins (SQS, NATS, Azure Service Bus, gRPC, …) follow the same two archetypes (`*-duplicator` / `*-writer`) and can be added by the user via their own `InceptionPlugin` CRDs.

**Archetype summary:**

- **`*-duplicator`** — reads from a source queue, duplicates matched messages to a shadow queue blue consumes from (ingress) or subscribes to an output queue blue produces to (egress).
- **`*-writer`** — exposes an HTTP `/write` endpoint; the test container calls it to publish messages into blue's input or output flow.
- **`http-proxy`** — sidecar that intercepts blue's HTTP traffic (ingress, egress, or both).
- **`http-writer`** — standalone gateway that forwards requests into blue's HTTP ingress (or to an upstream).

### Plugin Deployment Diagrams

**HTTP proxy (sidecar in blue pod):**

```mermaid
flowchart TD
    subgraph BP["Blue Pod"]
        BC["blue container"]
        PXY["http-proxy sidecar<br/>reads config from ConfigMap"]
        BC -->|"envVarName<br/>points to localhost:{port}"| PXY
    end

    SRC["HTTP Client"] -->|"ingress request"| PXY
    PXY -->|"ingress: forward to blue"| BC
    BC -->|"egress: outbound call"| PXY
    PXY -->|"trigger: register testId"| OP["Operator"]
    PXY -->|"observe / mock"| TC["Test Container"]
    PXY -->|"passthrough: forward"| REAL["Real Upstream"]
```

**Queue duplicator (standalone deployment):**

```mermaid
flowchart TD
    DUP["Duplicator Deployment<br/>stateless, reads ConfigMap"]
    SRC["Source Queue"] --> GREEN["Green consumes normally"]
    SRC --> DUP
    DUP -->|"matched messages"| SHQ["Shadow Queue"]
    SHQ --> BLUE["Blue"]
    DUP -->|"register testId"| OP["Operator"]
    DUP -->|"POST /trigger"| TC["Test Container"]
```

**Write gateway (standalone deployment):**

```mermaid
flowchart LR
    TC["Test Container"] -->|"call $WRITE_URL/write"| WR["Writer Deployment"]
    WR -->|"publish / request"| BLUE["Blue's queue / HTTP ingress"]
```

---

## State Store

The state store is the operator's persistence for active test runs (testIds, timeouts, verdicts, counts). It is configured — not built as a container plugin — because state-store access is latency-sensitive and the operator reads/writes it in-process.

A cluster admin picks a backend by creating a `StateStore` resource; a `BlueGreenDeployment` references it via `stateStoreRef`.

| Backend | Description | When to Use |
|---|---|---|
| `memory` | In-process hash map | Development / tests only |
| `redis` | Redis hash + sorted set for timeouts | Production; low-latency; built-in TTL |
| `postgres` | PostgreSQL table with TTL column | Production; durable; queryable |

All backends implement the same Rust trait; adding a backend is a matter of implementing the trait and extending the `StateStore` CRD's `type` enum.

```rust
trait StateStore: Send + Sync {
    async fn register(&self, run: InceptionTest) -> Result<()>;
    async fn get(&self, test_id: &str) -> Result<Option<InceptionTest>>;
    async fn set_verdict(&self, test_id: &str, passed: bool) -> Result<()>;
    async fn mark_timed_out(&self, test_id: &str) -> Result<()>;
    async fn list_pending(&self) -> Result<Vec<InceptionTest>>;
    async fn counts(&self, bg: &str) -> Result<Counts>;
    async fn cleanup_expired(&self) -> Result<usize>;
}
```

---

## Custom Resource Definitions

All three CRDs live in the `fluidbg.io/v1alpha1` API group.

### `InceptionPlugin`

Declares a plugin: its image, topology, what it supports, the schema of user config, and the template used to produce the plugin ConfigMap.

```yaml
apiVersion: fluidbg.io/v1alpha1
kind: InceptionPlugin
metadata:
  name: http-proxy
spec:
  description: "HTTP proxy sidecar for intercepting and routing HTTP traffic"
  image: fluidbg/http-proxy:v0.1.0
  topology: sidecar-blue

  modes: [trigger, passthrough-duplicate, reroute-mock]
  directions: [ingress, egress]
  fieldNamespaces: [http]

  # JSON Schema that validates the user's `config` block in the BlueGreenDeployment.
  # The operator rejects BlueGreenDeployments whose config does not match this schema.
  configSchema:
    type: object
    required: [proxyPort, realEndpoint, envVarName]
    properties:
      proxyPort: { type: integer }
      realEndpoint: { type: string }
      envVarName: { type: string }
      testId: { type: object }
      match: { type: array }
      filters: { type: array }
      ingress: { type: object }
      egress: { type: object }

  # Optional: Go-template-style transform from user `config` → plugin ConfigMap.
  # Omit for identity (operator writes user's config verbatim plus standard fields).
  configTemplate: ""

  container:
    ports:
      - name: proxy
        containerPort: 8080
    volumeMounts:
      - name: plugin-config
        mountPath: /etc/fluidbg
        readOnly: true

  # What this plugin injects into other containers in the BlueGreenDeployment.
  injects:
    blueContainer:
      env:
        - nameFromConfig: envVarName                   # key in user's config whose value is the env var name
          valueTemplate: "http://localhost:{{ .proxyPort }}"  # resolved by the operator
    testContainer:
      env: []
```

### `StateStore`

```yaml
apiVersion: fluidbg.io/v1alpha1
kind: StateStore
metadata:
  name: default
spec:
  type: postgres              # memory | redis | postgres
  postgres:
    url: "postgres://fluidbg:secret@postgres.fluidbg:5432/fluidbg"
    tableName: "fluidbg_cases"
    ttlSeconds: 86400
```

Only the sub-block matching `type` is read; others are ignored.

### `BlueGreenDeployment`

Below is a focused example. See the [Test Container Contract](#test-container-contract) for how the test container is wired.

```yaml
apiVersion: fluidbg.io/v1alpha1
kind: BlueGreenDeployment
metadata:
  name: order-processor
spec:
  stateStoreRef:
    name: default

  green:
    deployment: { name: order-processor-green, namespace: production }

  blue:
    deployment: { name: order-processor-blue, namespace: production }
    template:
      spec:
        containers:
          - name: order-processor
            image: myregistry/order-processor:v2.0.0-rc1

  inceptionPoints:
    # Trigger: a RabbitMQ message creates a test run.
    - name: incoming-orders
      directions: [ingress]
      mode: trigger
      pluginRef: { name: rabbitmq-duplicator }
      config:
        inputQueue: orders
        blueInputQueue: orders-blue
        testId:
          field: "queue.body"
          jsonPath: "$.orderId"
        match:
          - field: "queue.body"
            jsonPath: "$.type"
            matches: "^order$"
        notifyPath: "/trigger"
      notifyTests: [order-validation]
      timeout: 60s

    # Passthrough-duplicate: observe egress HTTP calls to the payment service.
    - name: payment-calls
      directions: [egress]
      mode: passthrough-duplicate
      pluginRef: { name: http-proxy }
      config:
        proxyPort: 8081
        realEndpoint: https://payment.internal/v1
        envVarName: PAYMENT_SERVICE_URL
        testId:
          field: "http.body"
          jsonPath: "$.orderId"
        filters:
          - match:
              - field: "http.path"
                matches: "^/v1/charge$"
            notifyPath: "/observe/{testId}/payment-charge"
            payload: both
      notifyTests: [order-validation]

    # Reroute-mock: test container mocks the inventory service.
    - name: inventory-calls
      directions: [egress]
      mode: reroute-mock
      pluginRef: { name: http-proxy }
      config:
        proxyPort: 8082
        realEndpoint: https://inventory.internal/v2
        envVarName: INVENTORY_SERVICE_URL
        testId:
          field: "http.body"
          jsonPath: "$.orderId"
        filters:
          - match:
              - field: "http.path"
                matches: "^/v2/reserve$"
            notifyPath: "/observe/{testId}/inventory-reserve"
            payload: both
      notifyTests: [order-validation]

    # Write: the test container can publish messages into blue's input queue.
    - name: order-inject
      directions: [ingress]
      mode: write
      pluginRef: { name: rabbitmq-writer }
      config:
        targetQueue: orders-blue
        writeEnvVar: ORDER_INJECT_URL
      notifyTests: [order-validation]

  tests:
    - name: order-validation
      image: myregistry/order-processor-tests:v2.0.0-rc1
      port: 8080
      triggerPath: /trigger
      resultPath: /result/{testId}
      env:
        - name: EXPECT_MAX_PROCESSING_TIME_MS
          value: "5000"

  promotion:
    successCriteria:
      minCases: 100
      successRate: 0.98
      observationWindowMinutes: 30

    strategy:
      type: progressive
      progressive:
        steps:
          - { trafficPercent: 5,   observeCases: 20,  successRate: 0.99 }
          - { trafficPercent: 25,  observeCases: 50,  successRate: 0.98 }
          - { trafficPercent: 50,  observeCases: 100, successRate: 0.98 }
          - { trafficPercent: 100, observeCases: 50,  successRate: 0.98 }
        rollbackOnStepFailure: true
        stepTimeoutMinutes: 15

status:
  phase: Observing
  casesObserved: 142
  casesPassed: 140
  casesFailed: 2
  casesPending: 3
  casesTimedOut: 1
  currentSuccessRate: 0.9859
  currentTrafficPercent: 25
  currentStep: 1
  startedAt: "2026-04-20T10:00:00Z"
  lastCaseAt: "2026-04-20T10:28:14Z"
```

#### Per-filter `payload` option

| Value | Meaning |
|---|---|
| `request` | Forward only the request to the test container |
| `response` | Forward only the response (after upstream call) |
| `both` (default) | Forward request and response |

For `trigger` and `write` modes, `payload` is ignored (no response exists).

#### `notifyPath` template variables

| Variable | Expands to |
|---|---|
| `{testId}` | The extracted `testId` for this case |
| `{inceptionPoint}` | The name of the inception point |
| `{direction}` | `ingress` or `egress` (in bidirectional plugins) |

---

## Test Container Contract

Test containers are user-supplied HTTP servers that hold all observation state and answer one question per test run: **green or not green**.

### Endpoints

| Endpoint | Called By | Purpose |
|---|---|---|
| `POST {triggerPath}` | Trigger plugins | "A new test run has started for this testId" |
| `POST {notifyPath}` | Passthrough-duplicate / reroute-mock plugins | Observation event; `reroute-mock` uses the response as the mock reply to blue |
| `POST {writerService}/write` | The test container itself (the URL is injected as an env var) | Inject traffic into blue |
| `GET {resultPath}` | The operator | Request a verdict for a specific testId |

### Trigger

```
POST /trigger
{ "testId": "ORD-123", "inceptionPoint": "incoming-orders",
  "payload": { ... }, "timestamp": "2026-04-20T10:00:00Z" }
```

### Observation

```
POST /observe/ORD-123/payment-charge
{ "inceptionPoint": "payment-calls", "direction": "egress",
  "mode": "passthrough-duplicate",
  "request":  { "method": "POST", "path": "/v1/charge", "headers": {...}, "body": {...} },
  "response": { "status": 200, "headers": {...}, "body": {...} },
  "timestamp": "2026-04-20T10:00:01Z" }
```

For `reroute-mock`, the test container's **response body** is the mock the plugin returns to blue:

```json
{ "status": "reserved", "quantity": 100 }
```

### Write (injected env vars)

The operator injects one env var per `write` inception point:

```
ORDER_INJECT_URL=http://fluidbg-writer-order-inject.production:9090
```

The test container calls `POST $ORDER_INJECT_URL/write` with a plugin-specific payload.

### Result

```
GET /result/ORD-123
→ { "testId": "ORD-123", "passed": true }
→ { "testId": "ORD-123", "passed": false }
→ { "testId": "ORD-123", "passed": null, "pending": ["inventory-reserve"] }
```

Only `testId` and `passed` are returned. If `passed` is `null`, the operator retries until the trigger's timeout elapses.

---

## Tracking & Correlation

The operator's `inception` module tracks testIds, enforces timeouts, and counts verdicts via the configured state store.

```rust
struct InceptionTest {
    test_id: String,
    blue_green_ref: String,            // which BlueGreenDeployment
    triggered_at: DateTime<Utc>,
    trigger_inception_point: String,
    timeout: Duration,
    status: TestStatus,                // Triggered | Observing | Passed | Failed | TimedOut
    verdict: Option<bool>,             // Some(true)=green, Some(false)=not-green, None=pending
}
```

### State Machine

```mermaid
stateDiagram-v2
    [*] --> Triggered : trigger plugin matches,<br/>extracts testId,<br/>registers with operator
    Triggered --> Observing : test container receives trigger
    Observing --> Passed : test container returns passed=true
    Observing --> Failed : test container returns passed=false
    Observing --> TimedOut : no verdict before timeout
    Passed --> [*] : counts.passed++
    Failed --> [*] : counts.failed++
    TimedOut --> [*] : counts.timedOut++
```

The operator periodically polls `GET /result/{testId}` on the test container. The evaluator divides `passed / (passed + failed + timedOut)` and compares against the user-defined `successRate`.

---

## Message Flow Sequence

```mermaid
sequenceDiagram
    participant RD as rabbitmq-duplicator<br/>(trigger)
    participant BC as Blue
    participant HP as http-proxy<br/>(passthrough-duplicate / reroute-mock)
    participant TC as Test Container
    participant OP as Operator

    RD->>RD: message arrives, match filters,<br/>extract testId="ORD-123"
    RD->>OP: POST /cases { testId, timeout=60s }
    RD->>TC: POST /trigger
    RD->>BC: publish to shadow queue

    BC->>HP: POST /v1/charge
    HP->>TC: POST /observe/ORD-123/payment-charge
    HP->>BC: forward real response

    BC->>HP: POST /v2/reserve
    HP->>TC: POST /observe/ORD-123/inventory-reserve
    TC-->>HP: { status: "reserved", quantity: 100 }
    HP-->>BC: mock response

    Note over TC: test container may POST $ORDER_INJECT_URL/write to drive traffic

    OP->>TC: GET /result/ORD-123 (polling)
    TC-->>OP: { passed: true }
    Note over OP: counts.passed++<br/>evaluator checks threshold
```

---

## Promotion

The user defines a success-rate threshold and a strategy.

### Success Criteria (Global)

```yaml
successCriteria:
  minCases: 100               # don't decide until this many cases have verdicts
  successRate: 0.98           # threshold for "green"
  observationWindowMinutes: 30
```

### Progressive Strategy

Traffic shifts to blue in steps; each step has its own minimum-case count and threshold.

```mermaid
flowchart TD
    OBS["Observing at current step"]
    OBS -->|"minCases met<br/>successRate ≥ threshold"| DEC["Last step?"]
    OBS -->|"successRate < threshold"| RB["Rollback"]
    DEC -->|yes| DONE["Completed (promoted)"]
    DEC -->|no| NEXT["Advance to next step<br/>(more traffic to blue)"]
    NEXT --> OBS
```

Traffic split is implemented:
- **Queue plugins**: the trigger duplicator routes a percentage of matched messages to the shadow queue (blue); the rest bypass, going only to green.
- **HTTP plugins**: the proxy splits traffic to blue vs green by configured percentage.

### Hard Switch Strategy

```mermaid
flowchart TD
    OBS2["Observing (0% traffic to blue)"]
    OBS2 -->|"minCases met &<br/>successRate ≥ threshold"| PROM["Promote:<br/>swap blue → green"]
    OBS2 -->|"successRate < threshold or timeout"| RB2["Rollback:<br/>tear down blue"]
```

### Comparison

| | Progressive | Hard Switch |
|---|---|---|
| Risk | Lower — problems caught at low percentages | Higher — full cutover at once |
| Speed | Slower — multiple windows | Faster — one window |
| Complexity | Higher — traffic splitting, multi-step evaluation | Lower — binary decision |

---

## Project Structure

```
src/
├── main.rs                     # Entrypoint, CRD registration, controller startup
├── crd/
│   ├── mod.rs
│   ├── blue_green.rs           # BlueGreenDeployment
│   ├── inception_plugin.rs     # InceptionPlugin
│   └── state_store.rs          # StateStore
├── controller.rs               # Reconcile loop: BGD → state machine
├── inception.rs                # Case tracking, timeouts, verdict polling
├── evaluator.rs                # Reads counts, applies thresholds, drives strategy
├── strategy/
│   ├── mod.rs                  # PromotionStrategy trait
│   ├── progressive.rs
│   └── hard_switch.rs
├── plugins/
│   ├── mod.rs                  # Plugin registry (loaded from InceptionPlugin CRDs)
│   ├── fields.rs               # Field reference resolution
│   ├── filter.rs               # Filter engine (equals / matches / jsonPath)
│   ├── selector.rs             # Selector engine
│   ├── schema.rs               # JSON-Schema validation of user config vs plugin configSchema
│   ├── template.rs             # configTemplate rendering (optional)
│   ├── reconciler.rs           # Generic: InceptionPlugin CRD + inception point → K8s resources
│   └── builtin_crds/           # YAML manifests for shipped plugins
│       ├── http_proxy.yaml
│       ├── http_writer.yaml
│       ├── rabbitmq_duplicator.yaml
│       └── rabbitmq_writer.yaml
├── state_store/
│   ├── mod.rs                  # StateStore trait + factory (reads StateStore CRD)
│   ├── memory.rs
│   ├── redis.rs
│   └── postgres.rs
├── http_api.rs                 # Operator HTTP API (POST /cases, etc.)
├── test_runner.rs              # Test container deployment, env injection, result polling
└── status.rs                   # Status/conditions helpers

plugins/                        # Plugin container source trees (built to separate images)
├── http_proxy/
├── http_writer/
├── rabbitmq_duplicator/
└── rabbitmq_writer/
```

**Key invariant:** `src/plugins/reconciler.rs` is plugin-agnostic. It reads an `InceptionPlugin` CRD and produces K8s resources. Adding a new plugin = new CRD YAML + new container image.

---

## Dependencies

| Crate | Purpose |
|---|---|
| `kube`, `k8s-openapi`, `schemars` | Kubernetes client, controller runtime, CRD derive |
| `tokio` | Async runtime |
| `tracing`, `tracing-subscriber` | Structured logging |
| `serde`, `serde_json`, `serde_yaml` | Serialization |
| `jsonschema` | Validate user config against plugin `configSchema` |
| `reqwest` | HTTP client (trigger test containers, poll results) |
| `axum` | HTTP server (operator API, writer gateways) |
| `jsonpath-rust` | JSONPath for selectors and body filters |
| `regex` | Filter regex matching |
| `sqlx` (postgres feature) | Postgres state store |
| `redis` | Redis state store |
| `thiserror`, `anyhow` | Errors |

Plugin containers have their own dependency sets (e.g., `lapin` for RabbitMQ plugins). They are not operator dependencies.

---

## Safety Guarantees

- **Green is never disrupted** — it keeps consuming from the source normally throughout.
- **Blue sees only mirrored traffic** in `passthrough-duplicate` — it cannot affect production state.
- **Reroute-mock isolates blue from real APIs** — blue's egress never reaches production upstreams.
- **Write endpoints only target the shadow path** — injected messages/requests reach blue, not green.
- **Progressive shifting limits blast radius** — failures are caught at small traffic percentages.
- **Rollback is atomic** — tear down blue, test containers, and all plugin deployments; green is untouched.
- **Timeouts prevent stuck cases** — every trigger has a user-defined timeout; unresolved cases become not-green.
- **Plugins are stateless** — ConfigMap + runtime env vars is the full plugin state; restart is safe.
- **State store is durable** — with `postgres` or `redis`, the operator recovers tracking state on restart.
- **CRD validation is strict** — unknown modes/directions, unsupported field namespaces, and configs that fail a plugin's `configSchema` are rejected before any resource is created.
