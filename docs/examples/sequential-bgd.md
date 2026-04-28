---
title: Sequential Order Flow Example
---

# Sequential Order Flow Example

The `examples/sequential-bgd` folder contains a small end-to-end order flow
demo. It is intentionally shaped like a normal service topology instead of a
test-only toy:

- a producer publishes incrementing order messages to RabbitMQ
- the input inceptor progressively routes each order to either the current app
  or the candidate app
- the selected application instance consumes the order
- the candidate application calls an HTTP audit endpoint through the HTTP
  inceptor during the rollout
- the application publishes result messages to RabbitMQ
- a downstream demo sink service receives both the HTTP calls and result
  messages
- the FluidBG verifier test container only asks the sink whether the candidate
  effects arrived

The sink is not the test container. It simulates another consumer that would be
present in a real system. Its logs are the main human-readable proof that the
rollout did not drop traffic.

## Topology

```mermaid
flowchart LR
    PRODUCER["order-flow-producer"]
    ORDERS["RabbitMQ: orders"]
    INCOMING["inceptor: incoming-orders<br/>RabbitMQ splitter + observer<br/>10% -> 50% -> 100% candidate"]
    BLUE_IN["RabbitMQ: orders-blue"]
    GREEN_IN["RabbitMQ: orders-green"]
    BLUE_APP["candidate app<br/>OUTPUT_PREFIX=v2"]
    GREEN_APP["current app<br/>OUTPUT_PREFIX=v1"]
    HTTP["inceptor: audit-http<br/>HTTP observer + proxy<br/>candidate path during rollout"]
    OUTGOING["inceptor: outgoing-results<br/>RabbitMQ combiner"]
    BLUE_OUT["RabbitMQ: results-blue"]
    GREEN_OUT["RabbitMQ: results-green"]
    RESULTS["RabbitMQ: results"]
    SINK["order-flow-sink<br/>normal downstream service"]
    VERIFIER["verifier test container"]
    OPERATOR["fbg-operator"]

    PRODUCER --> ORDERS
    ORDERS --> INCOMING
    INCOMING -->|"candidate route"| BLUE_IN
    INCOMING -->|"current route"| GREEN_IN
    BLUE_IN --> BLUE_APP
    GREEN_IN --> GREEN_APP
    BLUE_APP --> HTTP
    HTTP -->|"POST /audit"| SINK
    BLUE_APP --> BLUE_OUT
    GREEN_APP --> GREEN_OUT
    BLUE_OUT --> OUTGOING
    GREEN_OUT --> OUTGOING
    OUTGOING --> RESULTS
    RESULTS --> SINK
    INCOMING -->|"notify /observe/{testId}/incoming-orders"| VERIFIER
    HTTP -->|"notify /observe/{testId}/audit-http"| VERIFIER
    OPERATOR -->|"creates inceptors + verifier"| INCOMING
    OPERATOR --> HTTP
    OPERATOR --> OUTGOING
    OPERATOR --> VERIFIER
    OPERATOR -->|"polls /result/{testId}"| VERIFIER
    VERIFIER -->|"GET /cases/{testId}"| SINK
```

## Files

- `01-base.yaml` creates the demo namespace, RabbitMQ, the producer, the
  downstream sink, and the initial `BlueGreenDeployment` with `OUTPUT_PREFIX=v1`.
- `02-upgrade.yaml` updates the same `BlueGreenDeployment` to
  `OUTPUT_PREFIX=v2`, defines the verifier with native Kubernetes
  `deployment`/`service` specs, and enables the RabbitMQ and HTTP inception
  points.
- `app/`, `producer/`, `sink/`, and `verifier/` contain the small demo
  container images.

## Images

The example YAML uses published GHCR images:

- `ghcr.io/dlahmad/fluidbg-example-order-app:latest`
- `ghcr.io/dlahmad/fluidbg-example-producer:latest`
- `ghcr.io/dlahmad/fluidbg-example-sink:latest`
- `ghcr.io/dlahmad/fluidbg-example-verifier:latest`

For local development, build and load replacement images with the same tags:

```bash
docker build -t ghcr.io/dlahmad/fluidbg-example-order-app:latest examples/sequential-bgd/app
docker build -t ghcr.io/dlahmad/fluidbg-example-producer:latest examples/sequential-bgd/producer
docker build -t ghcr.io/dlahmad/fluidbg-example-sink:latest examples/sequential-bgd/sink
docker build -t ghcr.io/dlahmad/fluidbg-example-verifier:latest examples/sequential-bgd/verifier

KIND_CLUSTER="$(kind get clusters | head -n 1)"

kind load docker-image ghcr.io/dlahmad/fluidbg-example-order-app:latest --name "$KIND_CLUSTER"
kind load docker-image ghcr.io/dlahmad/fluidbg-example-producer:latest --name "$KIND_CLUSTER"
kind load docker-image ghcr.io/dlahmad/fluidbg-example-sink:latest --name "$KIND_CLUSTER"
kind load docker-image ghcr.io/dlahmad/fluidbg-example-verifier:latest --name "$KIND_CLUSTER"
```

The operator and built-in plugin images must also be available in the cluster.
For local development, build/load them with the repository scripts. For a
published release, use the GHCR defaults from the Helm chart.

This demo uses local RabbitMQ credentials because it creates a disposable broker
in the demo namespace. Credentials are plugin installation/runtime config, not
BGD config. The chart renders local values into Secrets first and injects them
via `secretKeyRef`; production installs should reference existing Secrets.

## Install Operator

Install the Helm chart into the system namespace and register the built-in
plugins in the demo namespace:

```bash
helm upgrade --install fluidbg charts/fluidbg-operator \
  --namespace fluidbg-system \
  --create-namespace \
  --set operator.auth.createSigningSecret=true \
  --set operator.auth.signingSecretName=fluidbg-operator-auth \
  --set operator.auth.signingSecretValue=dev-signing-key-change-me \
  --set builtinPlugins.rabbitmq.manager.enabled=true \
  --set builtinPlugins.rabbitmq.manager.amqpUrl='amqp://fluidbg:fluidbg@rabbitmq.fluidbg-demo:5672/%2f' \
  --set builtinPlugins.rabbitmq.manager.managementUrl='http://rabbitmq.fluidbg-demo:15672' \
  --set builtinPlugins.rabbitmq.manager.managementUsername=fluidbg \
  --set builtinPlugins.rabbitmq.manager.managementPassword=fluidbg \
  --set builtinPlugins.rabbitmq.manager.managementVhost='/' \
  --set builtinPlugins.namespaces[0]=fluidbg-demo
```

## Run The Demo

Apply the initial version:

```bash
kubectl apply -f examples/sequential-bgd/01-base.yaml
GEN=$(kubectl get bgd order-flow -n fluidbg-demo -o jsonpath='{.metadata.generation}')
kubectl wait --for=jsonpath='{.status.observedGeneration}'="$GEN" bgd/order-flow -n fluidbg-demo --timeout=180s
kubectl wait --for=jsonpath='{.status.rolloutGeneration}'="$GEN" bgd/order-flow -n fluidbg-demo --timeout=180s
kubectl wait --for=jsonpath='{.status.phase}'=Completed bgd/order-flow -n fluidbg-demo --timeout=180s
```

Apply the upgraded version:

```bash
kubectl apply -f examples/sequential-bgd/02-upgrade.yaml
GEN=$(kubectl get bgd order-flow -n fluidbg-demo -o jsonpath='{.metadata.generation}')
kubectl wait --for=jsonpath='{.status.observedGeneration}'="$GEN" bgd/order-flow -n fluidbg-demo --timeout=300s
kubectl wait --for=jsonpath='{.status.rolloutGeneration}'="$GEN" bgd/order-flow -n fluidbg-demo --timeout=300s
kubectl wait --for=jsonpath='{.status.phase}'=Completed bgd/order-flow -n fluidbg-demo --timeout=300s
```

Useful checks:

```bash
kubectl get bgd order-flow -n fluidbg-demo -o wide
kubectl logs -n fluidbg-demo deploy/order-flow-producer
kubectl logs -n fluidbg-demo deploy/order-flow-sink --tail=300
kubectl logs -n fluidbg-demo -l fluidbg.io/test-name=verifier --tail=100
kubectl get deploy -n fluidbg-demo --show-labels
```

Leave the demo running briefly after `Completed` so the post-promotion cleanup
path is visible. This exercises the path where temporary queue messages have
been moved back and the promoted app is patched away from the HTTP inceptor to
the real sink endpoint:

```bash
sleep 30
kubectl logs -n fluidbg-demo deploy/order-flow-sink --tail=300
kubectl exec -n fluidbg-demo deploy/order-flow-sink -- \
  python -c 'import json, urllib.request; print(json.dumps(json.load(urllib.request.urlopen("http://127.0.0.1:8080/summary")), indent=2, sort_keys=True))'
```

## What To Look For

The producer emits incrementing `sequence` values. The sink logs both sides of
the effect:

```text
HTTP sequence=42 order=demo-producer-42 prefix=v2 complete=False
OUTPUT sequence=42 order=demo-producer-42 result=v2-demo-producer-42 complete=True
OUTPUT STREAM OK allOutputMissing=[] prefix=v2 candidateCompleteCount=7
```

The verifier test container passes a test case only after this separate sink
service has observed both the HTTP audit call and the output message for the
candidate `v2` case. Prefix-specific gaps are expected with splitter routing,
because `v1` and `v2` each receive only their routed subset. The no-loss proof
is the combined output stream across both prefixes: `OUTPUT STREAM OK` and
`/summary.allOutputMissing == []`.

`trafficPercent` is the candidate-side percentage. The RabbitMQ splitter hashes
the message body and sends it to the candidate/blue queue when the hash falls
inside the configured percentage bucket. The decision is deterministic for a
given message body. It is updated through the plugin traffic endpoint, so step
changes do not restart the inceptor pod.

## Inception Points

`02-upgrade.yaml` uses three inception points:

- `incoming-orders` uses the RabbitMQ plugin as splitter and observer. It routes
  incoming orders into the current or candidate temporary queue based on the
  current progressive traffic percentage and registers test cases only for
  candidate-routed messages. It sets `temporaryQueueIdentifier:
  "incoming-orders"` so temporary input queues are easy to recognize in
  RabbitMQ.
- `audit-http` uses the HTTP plugin as observer. The application calls the
  plugin-provided endpoint through `HTTP_UPSTREAM`, while the plugin forwards to
  the normal downstream sink service.
- `outgoing-results` uses the RabbitMQ plugin as combiner. It combines candidate
  output messages back into the stable `results` queue consumed by the sink. It
  sets `temporaryQueueIdentifier: "outgoing-results"` for the temporary output queues.

Promotion is progressive: candidate traffic starts at 10%, moves to 50%, then
to 100%. Every step requires a success rate of `1.0` for observed candidate
cases before the next percentage is applied.
