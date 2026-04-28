---
title: Sequential Order Flow Example
---

# Sequential Order Flow Example

The `examples/sequential-bgd` folder contains a small end-to-end order flow
demo. It is intentionally shaped like a normal service topology instead of a
test-only toy:

- a producer publishes incrementing order messages to RabbitMQ
- the application under rollout consumes those orders
- the application calls an HTTP audit endpoint
- the application publishes result messages to RabbitMQ
- a downstream demo sink service receives both the HTTP calls and result
  messages
- the FluidBG verifier test container only asks the sink whether the candidate
  effects arrived

The sink is not the test container. It simulates another consumer that would be
present in a real system. Its logs are the main human-readable proof that the
rollout did not drop traffic.

## Files

- `01-base.yaml` creates the demo namespace, RabbitMQ, the producer, the
  downstream sink, and the initial `BlueGreenDeployment` with `OUTPUT_PREFIX=v1`.
- `02-upgrade.yaml` updates the same `BlueGreenDeployment` to
  `OUTPUT_PREFIX=v2` and enables the RabbitMQ and HTTP inception points.
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

kind load docker-image ghcr.io/dlahmad/fluidbg-example-order-app:latest --name <kind-cluster>
kind load docker-image ghcr.io/dlahmad/fluidbg-example-producer:latest --name <kind-cluster>
kind load docker-image ghcr.io/dlahmad/fluidbg-example-sink:latest --name <kind-cluster>
kind load docker-image ghcr.io/dlahmad/fluidbg-example-verifier:latest --name <kind-cluster>
```

The operator and built-in plugin images must also be available in the cluster.
For local development, build/load them with the repository scripts. For a
published release, use the GHCR defaults from the Helm chart.

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
  --set builtinPlugins.namespaces[0]=fluidbg-demo
```

## Run The Demo

Apply the initial version:

```bash
kubectl apply -f examples/sequential-bgd/01-base.yaml
kubectl wait --for=jsonpath='{.status.phase}'=Completed bgd/order-flow -n fluidbg-demo --timeout=180s
```

Apply the upgraded version:

```bash
kubectl apply -f examples/sequential-bgd/02-upgrade.yaml
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

## What To Look For

The producer emits incrementing `sequence` values. The sink logs both sides of
the effect:

```text
HTTP sequence=42 order=demo-producer-42 prefix=v2 complete=False
OUTPUT sequence=42 order=demo-producer-42 result=v2-demo-producer-42 complete=True
SEQUENCE OK prefix=v2 httpMissing=[] outputMissing=[] completeMissing=[]
```

The verifier test container passes a test case only after this separate sink
service has observed both the HTTP audit call and the output message for the
candidate `v2` case. If traffic is lost, the sink log would show a gap in the
HTTP stream, the output stream, or the completed HTTP+output pair stream.

## Inception Points

`02-upgrade.yaml` uses three inception points:

- `incoming-orders` uses the RabbitMQ plugin as duplicator and observer. It
  splits incoming orders into blue/green temporary queues and registers test
  cases from real messages.
- `audit-http` uses the HTTP plugin as observer. The application calls the
  plugin-provided endpoint through `HTTP_UPSTREAM`, while the plugin forwards to
  the normal downstream sink service.
- `outgoing-results` uses the RabbitMQ plugin as combiner. It combines candidate
  output messages back into the stable `results` queue consumed by the sink.

Promotion is allowed only after at least three candidate cases passed with a
success rate of `1.0`.
