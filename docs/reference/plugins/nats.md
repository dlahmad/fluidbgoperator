---
layout: page
title: NATS JetStream Plugin
---

# NATS JetStream Plugin

## Identity And Topology

| Field | Value |
|---|---|
| Built-in plugin name | `nats` |
| Image | `ghcr.io/dlahmad/fbg-plugin-nats` |
| Supported roles | `duplicator`, `splitter`, `combiner`, `observer`, `writer`, `consumer` |
| Progressive shifting | Supported for `splitter` |
| Manager mode | Supported and recommended |

The NATS plugin uses JetStream streams for reliable temporary transport state.
The manager runs in the operator namespace, owns stream creation/deletion, and
returns runtime env for per-inception inceptors. Application namespaces should
not receive the manager's administrative NATS credential. If your NATS account
uses separate limited credentials for app traffic, configure
`builtinPlugins.nats.manager.inceptorUrl`; otherwise the manager URL is reused.

```mermaid
flowchart TD
    SRC["base input subject"]
    MGR["nats manager<br/>operator namespace"]
    IN["input inceptor<br/>duplicator or splitter"]
    GS["green temp subject + stream"]
    BS["blue temp subject + stream"]
    GREEN["green app"]
    BLUE["blue app"]
    GO["green output subject + stream"]
    BO["blue output subject + stream"]
    COMB["combiner inceptor"]
    OUT["base output subject"]
    TEST["test container"]
    OP["operator"]

    MGR -->|"create/delete derived streams"| GS
    MGR -->|"create/delete derived streams"| BS
    SRC --> IN
    IN --> GS --> GREEN --> GO
    IN --> BS --> BLUE --> BO
    GO --> COMB
    BO --> COMB
    COMB --> OUT
    IN -->|"notify first"| TEST
    COMB -->|"notify first"| TEST
    IN -->|"register after notify"| OP
    COMB -->|"register after notify"| OP
```

## Configuration Reference

Top-level fields:

| Field | Required | Used By | Meaning |
|---|---|---|---|
| `stream` | no | manager/inceptor setup | JetStream stream properties for created streams. |
| `duplicator` | when role active | `duplicator` | Base input and green/blue input subjects plus env vars to patch. |
| `splitter` | when role active | `splitter` | Base input and green/blue input subjects plus env vars to patch. |
| `combiner` | when role active | `combiner` | Green/blue output subjects, base output subject, and env vars to patch. |
| `writer` | when role active | `writer` | Target subject for `/write`. |
| `consumer` | when role active | `consumer` | Input subject for consumer-style reads. |
| `observer` | when role active | `observer` | Test id selector, match filters, and verifier callback path. |

`stream` supports `subjects`, `storage`, `retention`, `replicas`,
`maxMessages`, `maxBytes`, `maxAgeSeconds`, `discard`, `allowRollup`,
`denyDelete`, `denyPurge`, `placement`, `persistenceMode`,
`subjectTransform`, and `metadata`. Stream names are always derived by the
plugin from the subject so each managed base or temporary subject maps to a
bounded, collision-resistant FluidBG-owned stream. If `stream.subjects` is set,
the active subject is added to that list so the generated stream still captures
the traffic the role uses.

`duplicator`, `splitter`, and `combiner` may set
`temporarySubjectIdentifier` to a semantic value up to 40 characters using
letters, digits, `.`, `_`, or `-`. The SDK converts it to a fixed
10-character safe token before including it in derived temporary subject names,
for example `fluidbg-green-in-incomiada9-<hash>`.

## Role Behavior

| Role | Behavior | Assignments |
|---|---|---|
| `duplicator` | Pulls from `duplicator.inputSubject` and publishes every message to green and blue temporary subjects. Route metadata is `both`. | Patches green and blue input subject and queue-group env vars. |
| `splitter` | Pulls from `splitter.inputSubject` and publishes each message to green or blue based on current candidate traffic percentage. | Patches green and blue input subject and queue-group env vars. |
| `combiner` | Pulls from green and blue output subjects, republishes to `combiner.outputSubject`, and derives route metadata from the source subject. | Patches green and blue output subject env vars. |
| `observer` | Applies `observer.match`, extracts `testId`, posts `observer.notifyPath`, then registers operator cases for `blue`, `both`, and `unknown` routes. | None. |
| `writer` | Exposes `/write` and publishes the supplied JSON payload to `writer.targetSubject`. | Test-container env injection can point callers to the writer service. |
| `consumer` | Pulls from `consumer.inputSubject` for plugin-driven read flows. | None. |

`duplicator`, `splitter`, `combiner`, and `consumer` are mutually exclusive
movement roles for one NATS inceptor. `observer` and `writer` are additive.

For queue-like no-loss semantics, configure `queueGroup`,
`greenQueueGroup`, `blueQueueGroup`, `greenQueueGroupEnvVar`, and
`blueQueueGroupEnvVar` on `duplicator` or `splitter` roles. The application
should consume JetStream with a durable derived from the subject plus the
current queue-group env var. The inceptor uses the same base queue-group
durable while it is active, so it work-shares with the current green app
instead of creating an independent replaying subscription. During drain, it
uses the green/blue durable names to detect messages already delivered to app
pods and to move only still-pending temporary work back to the base subject.

## Runtime State Machine

```mermaid
stateDiagram-v2
    [*] --> Idle: pod starts
    Idle --> Prepared: operator POST /prepare creates streams and returns assignments
    Prepared --> Active: operator POST /activate after verifier and app rollouts are ready
    Active --> Active: pull, publish, observe, write
    Active --> Active: operator POST /traffic
    Active --> Draining: operator POST /drain
    Draining --> Draining: move temp stream messages back
    Draining --> Drained: temp streams have zero messages
    Draining --> TimedOutMaybeSuccessful: operator drain timeout
    Drained --> Cleaned: operator POST /cleanup
    TimedOutMaybeSuccessful --> Cleaned: cleanup with explicit risk
```

The source message is acknowledged only after required downstream publish work
and verifier notification have succeeded. Plugin-owned acknowledgements use
JetStream confirmed ACKs, so drain status is based on server-observed consumer
state instead of a best-effort client send. If an error occurs first, the
message is left unacknowledged so JetStream can redeliver according to its
consumer policy.

## Failure Behavior

| Situation | Behavior |
|---|---|
| Stream creation fails | `prepare` fails; the operator retries reconciliation and the rollout does not enter `Observing`. |
| Verifier readiness is slow or app rollout is still updating | The plugin stays idle and does not pull from base subjects. Existing green traffic continues through the old wiring. |
| Downstream publish fails | The source message is not acknowledged; JetStream can redeliver. |
| Verifier notification fails after retry | The operator case is not registered, preventing false promotion counts. |
| Operator registration fails after verifier notification | The plugin logs the error. The case is not counted until registration succeeds on a later delivery. |
| Green-only progressive observation | The verifier may be notified, but no operator case is registered. |
| Drain timeout | The operator records `TimedOutMaybeSuccessful` and proceeds with cleanup. |

## Drain And Cleanup

During drain, input roles stop pulling new base-subject work and move available
messages from temporary green/blue streams back to the base subject. They use
the configured green/blue queue-group durables, so messages already delivered
to an app pod remain visible as ack-pending until the app acknowledges or
JetStream redelivers them. Combiner roles move temporary output stream
messages back to the base output subject using the same durable the active
combiner uses. Drain status returns success only after all relevant temporary
durables have zero pending and zero ack-pending messages.

Cleanup deletes only derived stream names recomputed from token claims and
active roles. Derived subjects and stream names include purpose plus a bounded
hash; user-supplied subject names are not trusted for manager cleanup. The
manager also supports `/manager/sync`, allowing the operator to send active
inception inventory so missed temporary streams can be garbage-collected after
manager restarts.

## Security Boundary

The manager verifies the per-inception JWT and derives namespace, BGD,
inception point, and plugin identity from claims. NATS URLs are plugin
installation/runtime configuration, not BGD config. The BGD only describes
subjects, stream properties, role behavior, and verifier matching.

Configure the manager with:

| Helm value | Meaning |
|---|---|
| `builtinPlugins.nats.manager.url` or `urlSecretName`/`urlSecretKey` | Manager NATS URL used for JetStream stream management and publishing. |
| `builtinPlugins.nats.manager.inceptorUrl` | Optional limited runtime URL returned to inceptor pods as `FLUIDBG_NATS_URL`. |
| `builtinPlugins.nats.manager.requireTls` | Sets `FLUIDBG_NATS_REQUIRE_TLS=true`. |
| `builtinPlugins.nats.manager.caCertPath` | PEM bundle path used by manager and inceptors as `FLUIDBG_NATS_CA_CERT_PATH`. |

Publicly trusted NATS TLS certificates work with `tls://` URLs without extra
configuration. For private CAs, mount the PEM bundle into the manager and
inceptor pods and set `caCertPath`. Control-plane TLS between operator,
manager, and inceptors is separate; use
`builtinPlugins.nats.manager.controlPlaneTls` and
`builtinPlugins.nats.controlPlaneTls`.
