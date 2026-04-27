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
    CRD["Regenerate CRDs<br/>copy into e2e deploy manifests"]
    IMG["Build musl binaries<br/>build fbg images"]
    LOAD["Load images into kind"]
    INFRA["Apply RabbitMQ, httpbin,<br/>CRDs, StateStore, operator"]
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

## Runtime Topology

```mermaid
flowchart LR
    TEST["e2e script"]
    K8S["kind cluster"]
    OP["fbg-operator"]
    RAB["RabbitMQ"]
    HTTPBIN["httpbin"]
    GREEN["green app"]
    BLUE["blue app"]
    TC["test app"]
    RP["fbg-plugin-rabbitmq"]
    HP["fbg-plugin-http"]

    TEST -->|"kubectl apply"| K8S
    K8S --> OP
    K8S --> RAB
    K8S --> HTTPBIN
    OP --> GREEN
    OP --> BLUE
    OP --> TC
    OP --> RP
    OP --> HP
    RP --> RAB
    HP --> HTTPBIN
    RP --> TC
    HP --> TC
    OP -->|"poll verifyPath"| TC
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
