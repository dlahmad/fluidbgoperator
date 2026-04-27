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
    OP -->|"manager prepare/cleanup<br/>Bearer per-inception JWT"| RM
    OP -->|"manager prepare/cleanup<br/>Bearer per-inception JWT"| AM
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
- Never trust BGD-provided temporary queue names for create/delete authority.

Inceptor rules:

- Receive `FLUIDBG_PLUGIN_AUTH_TOKEN`, not the signing key.
- Require incoming operator calls to use the same bearer token value.
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
    O->>I: create ConfigMap/Deployment/Service with token only
    O->>M: POST /manager/prepare + Bearer JWT
    M->>M: verify JWT signature
    M->>T: create derived temporary resources
    O->>I: POST /prepare + Bearer same JWT
    I->>I: exact token match
    I-->>O: assignments for app/test containers
    O->>A: patch env assignments
```

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

RabbitMQ drain waits for temporary queues to be empty and for input queues to
have no active consumers before cleanup. Azure Service Bus uses a stability
window because locked messages can become visible again after the lock expires.
If the configured drain timeout is exceeded, the operator records
`TimedOutMaybeSuccessful` instead of silently treating the drain as safe.

## Built-In Plugin Matrix

| Plugin | Manager | Inceptor | Progressive | Notes |
|---|---|---|---|---|
| HTTP | Not used | Combined splitter/observer/mock/writer service | Yes | No external resource-admin secret is needed. |
| RabbitMQ | Optional, recommended | Duplicator/splitter/combiner/observer/writer/consumer | Yes | Manager owns temp queue create/delete; inceptor moves messages. |
| Azure Service Bus | Optional, recommended | Duplicator/splitter/combiner/observer/writer/consumer | Yes | Manager supports connection string and workload identity modes. |

