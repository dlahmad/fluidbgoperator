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

### InceptionPlugin CRD

An `InceptionPlugin` declares:

- `supportedRoles`
- `topology`
- `lifecycle.preparePath`
- `lifecycle.cleanupPath`
- `configSchema`
- `fieldNamespaces`
- `features`

For the built-in RabbitMQ plugin the important roles are:

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

The plugin gets the full operator and test-container base URLs from env injection, not from hardcoded names inside the plugin itself.

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

### `POST /cleanup`

Called by the operator after `Completed` or `RolledBack`.

Purpose:

- delete transport-specific derived resources
- allow the operator to restore direct application wiring cleanly

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

    note1["Discovery sources:
- operator service DNS
- test service DNS
- BGD name
- inception point name
- verify path template"] --> POD
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
- routes messages to green or blue based on `FLUIDBG_TRAFFIC_PERCENT`
- progressive shifting is implemented by the operator changing that env value on the plugin deployment

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
- registers the `testCase` with the operator
- calls `observer.notifyPath` on the test container

## Practical Notes

- Standalone plugins and the test container are temporary rollout resources.
- Service discovery must be namespace-qualified when the caller is outside the test namespace.
- `testCasesObserved` only counts finalized cases.
- `testCasesPending` must be included when you want to know whether the operator has started tracking traffic already.
