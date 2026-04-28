---
title: Plugin Architecture
---

# Plugin Architecture

FluidBG plugins are split into two roles when privileged infrastructure control
is required:

- **Manager:** long-running control-plane process in the operator namespace.
- **Inceptor:** per-inception traffic process in the application namespace.

HTTP does not currently need a manager because it does not create privileged
external infrastructure. RabbitMQ and Azure Service Bus can use managers because
queue create/delete credentials must not be handed to application namespaces.

## Component Model

```mermaid
flowchart LR
    subgraph OPS["operator namespace"]
        OP["fbg-operator"]
        KEY["operator signing Secret"]
        RM["RabbitMQ manager"]
        AM["Azure Service Bus manager"]
    end

    subgraph APP["application namespace"]
        BGD["BlueGreenDeployment"]
        IPC["InceptionPlugin CR"]
        RI["RabbitMQ inceptor"]
        AI["Azure Service Bus inceptor"]
        HI["HTTP inceptor"]
        GREEN["green app"]
        BLUE["blue app"]
        TEST["test container"]
    end

    EXT["external transport<br/>RabbitMQ / Service Bus / HTTP"]

    BGD --> OP
    IPC --> OP
    OP -->|"reads signing key"| KEY
    OP -->|"creates ConfigMap/Deployment/Service"| RI
    OP -->|"creates ConfigMap/Deployment/Service"| AI
    OP -->|"creates ConfigMap/Deployment/Service"| HI
    OP -->|"manager prepare/cleanup/sync<br/>Bearer JWT"| RM
    OP -->|"manager prepare/cleanup/sync<br/>Bearer JWT"| AM
    OP -->|"inceptor lifecycle<br/>Bearer same JWT"| RI
    OP -->|"inceptor lifecycle<br/>Bearer same JWT"| AI
    OP -->|"inceptor lifecycle<br/>Bearer same JWT"| HI
    RI --> EXT
    AI --> EXT
    HI --> EXT
    RI --> GREEN
    RI --> BLUE
    HI --> GREEN
    HI --> BLUE
    RI --> TEST
    AI --> TEST
    HI --> TEST
```

## Trust Boundary

The application namespace is not trusted with infrastructure-admin credentials.
An attacker who can edit a `BlueGreenDeployment` in that namespace must not be
able to create or delete arbitrary queues by choosing matching names.

```mermaid
flowchart TD
    USER["namespace user controls BGD config"]
    OP["operator"]
    JWT["per-inception JWT claims<br/>namespace, BGD, inception point, plugin"]
    MGR["manager"]
    DERIVE["derive temp resource names"]
    EXT["external infrastructure"]
    INCEPTOR["inceptor"]

    USER -->|"untrusted config"| OP
    OP -->|"signs claims with operator Secret"| JWT
    OP -->|"manager request<br/>roles + secured config + Bearer JWT"| MGR
    MGR -->|"verify JWT signature"| JWT
    MGR -->|"ignore untrusted temp names"| DERIVE
    DERIVE -->|"create/delete derived resources only"| EXT
    OP -->|"secured config + token only"| INCEPTOR
    INCEPTOR -->|"no signing key<br/>exact bearer-token match only"| OP
```

Manager rules:

- Verify the JWT signature using the operator signing key.
- Trust `namespace`, `blueGreenRef`, `inceptionPoint`, and `plugin` only from
  token claims.
- Recompute derived temporary resource names from claims and active role names.
  Queue-style names include namespace, BGD name, BGD UID, inception point, role,
  and logical purpose so recreated BGDs and concurrent BGDs cannot collide.
- Never trust BGD-provided temporary queue names for create/delete authority.
- Return inceptor runtime environment only from authenticated manager prepare.
  The operator injects those values before creating the inceptor pod.
- Implement `syncPath` so the operator can periodically send the active
  inception inventory. Managers use that inventory to delete scoped credentials
  and plugin-owned temporary resources that missed normal cleanup because a
  manager or operator died at the wrong time.

Inceptor rules:

- Receive `FLUIDBG_PLUGIN_AUTH_TOKEN`, not the signing key.
- Require incoming operator calls to use the same bearer token value.
- Start idle and do not move traffic until the operator calls `activatePath`.
- Treat `preparePath` as setup and assignment discovery only. It must not
  consume from base queues, proxy HTTP calls, notify verifiers, register cases,
  or write output.
- Use `FLUIDBG_INCEPTOR_INFRA_DISABLED=true` to skip privileged create/delete
  operations when a manager is configured.
- Move traffic and perform observation using the secured config emitted by the
  operator.

## Prepare Flow

```mermaid
sequenceDiagram
    participant O as Operator
    participant K as Signing Secret
    participant M as Manager
    participant I as Inceptor
    participant T as Transport
    participant A as Green/Blue Apps

    O->>K: read signing key from operator namespace
    O->>O: sign per-inception JWT
    O->>O: derive secured temp resource names
    loop each inception point
        O->>M: POST /manager/prepare + Bearer JWT
        M->>M: verify JWT signature
        M->>T: create derived temporary resources
        M-->>O: inceptorEnv with scoped runtime access
        O->>I: create ConfigMap/Deployment/Service with token + inceptorEnv
        I->>I: stay idle until prepared
        O->>I: POST /prepare + Bearer same JWT
        I->>I: exact token match, setup only, remain idle
        I-->>O: assignments for app containers
    end
    O->>A: create candidate with blue assignments in initial pod template
    O->>O: create verifier with final test env and wait for readiness
    O->>A: patch all env assignments in one batch
    O->>O: wait for app rollouts
    loop each inception point
        O->>I: POST /activate + Bearer same JWT
        I->>I: start traffic work
    end
```

The operator intentionally batches app assignment patches after all inception
points have been prepared, then activates inceptors only after the app rollouts
are ready. This avoids partial wiring such as input traffic being redirected
while the same app still publishes to an old output queue.

## Traffic Flow

```mermaid
flowchart LR
    SRC["source traffic"]
    INC["inceptor"]
    GREEN["green app"]
    BLUE["blue app"]
    OUT["output/upstream"]
    TEST["test container"]
    OP["operator"]

    SRC --> INC
    INC -->|"green route"| GREEN
    INC -->|"blue route"| BLUE
    INC -->|"both route"| GREEN
    INC -->|"both route"| BLUE
    GREEN --> OUT
    BLUE --> OUT
    INC -->|"register testCase<br/>Bearer token"| OP
    INC -->|"notify observation<br/>route metadata"| TEST
    OP -->|"poll verifyPath"| TEST
```

Route metadata is plugin-owned. Applications do not need to put route fields in
message bodies or HTTP payloads. A duplicator reports `both`; a splitter reports
`green` or `blue`; a combiner derives route from the source queue or endpoint.

## Drain And Cleanup Flow

```mermaid
sequenceDiagram
    participant O as Operator
    participant I as Inceptor
    participant M as Manager
    participant T as Transport
    participant A as Apps

    O->>I: POST /drain
    I-->>O: restore assignments
    O->>A: restore direct wiring
    loop until drained or timeout
        O->>I: GET /drain-status
        I->>T: check/move temporary work
        I-->>O: drained true/false
    end
    O->>I: POST /cleanup
    I->>I: stop traffic work
    O->>M: POST /manager/cleanup + Bearer JWT
    M->>M: verify JWT signature
    M->>T: delete derived temporary resources
    O->>O: delete inceptor Deployments/Services/ConfigMaps/Pods
```

## Manager Sync

```mermaid
sequenceDiagram
    participant O as Operator orphan cleanup loop
    participant M as Manager
    participant K as Kubernetes API
    participant T as Transport

    O->>K: list active BGDs and inception points for plugin
    O->>O: sign manager-sync JWT
    O->>M: POST /manager/sync + activeInceptions
    M->>M: verify JWT and plugin identity
    M->>T: list scoped credentials/resources
    M->>T: delete entries not represented in activeInceptions
```

The sync request contains the active namespace, BGD name, BGD UID, inception
point, roles, and BGD plugin config. It deliberately does not contain privileged
transport credentials. Managers recompute derived temporary names from that
inventory and remove only resources with FluidBG-owned prefixes that are absent
from the active set.

Plugin-specific drain and failure details are intentionally documented once in
the built-in plugin references: [RabbitMQ](plugins/rabbitmq.md), [Azure Service
Bus](plugins/azure-servicebus.md), and [HTTP](plugins/http.md).

## Built-In Plugin Matrix

| Plugin | Manager | Inceptor | Progressive | Notes |
|---|---|---|---|---|
| HTTP | Not used | Combined splitter/observer/mock/writer service | Yes | No external resource-admin secret is needed. |
| RabbitMQ | Optional, recommended | Duplicator/splitter/combiner/observer/writer/consumer | Yes | Manager owns temp queue create/delete; inceptor moves messages. |
| Azure Service Bus | Optional, recommended | Duplicator/splitter/combiner/observer/writer/consumer | Yes | Manager supports connection string and workload identity modes. |
