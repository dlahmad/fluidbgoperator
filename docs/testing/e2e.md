---
title: E2E Test Flow
---

# E2E Test Flow

The e2e suite in `e2e/run-test.sh` is the executable system test for the
operator, built-in plugins, CRDs, and example applications. It runs against a
kind cluster and uses local dev images by default.

## Suite Flow

```mermaid
flowchart TD
    START["Run e2e/run-test.sh"]
    CRD["Regenerate CRDs<br/>copy into Helm chart"]
    IMG["Build musl binaries<br/>build fbg images"]
    LOAD["Load images into kind"]
    INFRA["Apply RabbitMQ/httpbin<br/>optional Postgres<br/>install CRDs, operator, plugins with Helm"]
    BOOT["Bootstrap BGD<br/>first green deployment"]
    PASS["Apply upgrade BGD<br/>send messages and HTTP calls"]
    VERIFY["Test container observes<br/>messages and REST calls"]
    DECIDE["Operator polls verifyPath<br/>and evaluates criteria"]
    CLEAN["Promotion or rollback<br/>drain and cleanup inception resources"]
    CHECK["Assert expected output,<br/>calls, statuses, and cleanup"]

    START --> CRD --> IMG --> LOAD --> INFRA --> BOOT --> PASS --> VERIFY --> DECIDE --> CLEAN --> CHECK
```

## Covered Scenarios

- Bootstrap from no existing green deployment.
- Successful queue-driven promotion.
- Rollback with queue-drain recovery.
- Progressive traffic shifting through a splitter plugin without restarting the plugin pod.
- Rejection of progressive strategy when the splitter plugin does not advertise `supportsProgressiveShifting`.
- Combined HTTP plugin proxy, observer, mock, and writer behavior.
- Multiple inception points in one test case, where both expected HTTP calls and expected output messages must be observed before success.
- Same-`BlueGreenDeployment` rollout serialization so a new rollout cannot start while previous inception resources still exist.
- Different `BlueGreenDeployment` names running without generated-name collisions.
- Forced-delete recovery for missing BGD CRs with finalizers removed.
- Optional HA state-store run with two operator replicas and Postgres.

## State Store Modes

The default e2e mode uses the in-memory state store and one operator replica:

```sh
KIND_CLUSTER=fluidbg-dev BUILD_IMAGES=1 ./e2e/run-test.sh
```

To verify the HA-safe backend path, run with Postgres and two operator replicas:

```sh
KIND_CLUSTER=fluidbg-dev BUILD_IMAGES=1 E2E_STATE_STORE=postgres OPERATOR_REPLICAS=2 ./e2e/run-test.sh
```

That mode deploys a local `postgres:18-alpine` instance, stores the connection
URL in `secret/fluidbg-postgres`, installs the operator through Helm with
`stateStore.type=postgres`, and waits for both operator replicas to become
ready. The chart intentionally rejects `OPERATOR_REPLICAS=2` with
`stateStore.type=memory`.

## Runtime Topology

```mermaid
flowchart LR
    TEST["e2e script"]
    K8S["kind cluster"]
    OP["fbg-operator"]
    RAB["RabbitMQ"]
    PG["Postgres<br/>optional HA run"]
    HTTPBIN["httpbin"]
    GREEN["green app"]
    BLUE["blue app"]
    TC["test app"]
    RM["fbg-plugin-rabbitmq manager"]
    RP["fbg-plugin-rabbitmq inceptors"]
    HP["fbg-plugin-http"]

    TEST -->|"helm install chart"| OP
    TEST -->|"kubectl apply test manifests"| K8S
    K8S --> OP
    K8S --> RAB
    K8S -.-> PG
    K8S --> HTTPBIN
    OP --> GREEN
    OP --> BLUE
    OP --> TC
    OP -->|"manager prepare/cleanup"| RM
    OP --> RP
    OP --> HP
    RM -->|"create/delete derived queues"| RAB
    RP --> RAB
    HP --> HTTPBIN
    RP --> TC
    HP --> TC
    OP -->|"poll verifyPath"| TC
    OP -.->|"shared test-case store"| PG
```

## Plugin Manager/Inceptor Path

```mermaid
sequenceDiagram
    participant E as e2e script
    participant H as Helm/kubectl manifests
    participant O as Operator
    participant M as RabbitMQ manager
    participant I as RabbitMQ inceptors
    participant R as RabbitMQ
    participant T as Test app

    E->>H: helm install chart
    H->>O: Helm creates CRDs, operator, auth Secret
    H->>M: Helm creates manager
    H->>I: Helm registers built-in InceptionPlugins
    O->>I: create per-inception Deployments with token only
    O->>M: POST /manager/prepare with signed JWT
    M->>R: create derived temp queues
    O->>I: POST /prepare with same JWT
    I->>R: duplicate/split/combine messages
    I->>T: notify observations
    I->>O: register test cases
    O->>T: poll verifyPath
    O->>I: drain and cleanup
    O->>M: POST /manager/cleanup with signed JWT
    M->>R: delete derived temp queues
```

## Success Signal

The suite does not accept a rollout just because the operator status reached a
terminal phase. The test app only returns success when every expected observation
for a test case has been seen. For combined queue/HTTP cases this means:

- The expected output message was emitted.
- The expected REST call was observed by the HTTP plugin.
- The observed events use plugin-supplied route metadata, not application-owned payload fields.

## Cleanup Checks

After terminal promotion or rollback, the suite checks that temporary inception
resources are gone. This belongs to the operator, not the test container. The
operator performs drain, cleanup, and Kubernetes resource deletion; the test only
asserts that those effects happened.

The suite also force-deletes a BGD after it has created candidate, test,
inception, and store state. It removes the finalizer before deletion, then waits
for the orphan cleanup loop to remove all `fluidbg.io/blue-green-ref` labeled
resources for that BGD. In Postgres mode it additionally asserts that no store
rows remain for the force-deleted BGD.

The suite also uninstalls the Helm release and asserts that chart-owned
operator resources are removed: operator Deployment/Service/ServiceAccount,
manager Deployment/Service, built-in `InceptionPlugin` resources, RBAC, and the
chart-created signing Secret. CRDs are installed from the chart but are deleted
explicitly at the beginning of the next e2e run because Helm intentionally does
not remove CRDs on uninstall.
