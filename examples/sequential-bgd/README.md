# Sequential Order Flow Demo

This example shows one blue-green rollout with live queue traffic:

- a producer publishes orders to RabbitMQ
- the input inceptor progressively routes each order to either the current app or the candidate app
- the selected application instance consumes the order
- the candidate application calls an HTTP audit endpoint through the HTTP inceptor during the rollout
- the application publishes an output message
- one normal downstream demo sink service receives and logs the HTTP audit calls and final output messages
- one verifier test container asks the sink whether the candidate effects arrived
- the operator increases candidate traffic only after the sink has seen both effects for candidate traffic

## Images

The example application images are not published release artifacts. Build them
locally and load them into kind with the tags used by the YAML:

```sh
./scripts/build-example-images.sh --registry fluidbg --tag dev

KIND_CLUSTER="$(kind get clusters | head -n 1)"

kind load docker-image fluidbg/fluidbg-example-order-app:dev --name "$KIND_CLUSTER"
kind load docker-image fluidbg/fluidbg-example-producer:dev --name "$KIND_CLUSTER"
kind load docker-image fluidbg/fluidbg-example-sink:dev --name "$KIND_CLUSTER"
kind load docker-image fluidbg/fluidbg-example-verifier:dev --name "$KIND_CLUSTER"
```

The sink is intentionally not the test container. It simulates a downstream
service that would already exist in a real system: it consumes the stable
`results` queue and exposes a stable HTTP audit endpoint. The verifier is a
separate operator-created test container that only queries the sink API to
decide whether candidate traffic is safe to promote.

## One-Time Setup

For a local kind demo, run the setup helper. It builds and loads the example
images, preloads the disposable RabbitMQ image, installs the operator and
built-in plugins with Helm, and creates `fluidbg-demo`. It does not apply the
BGD manifests, so you can apply the base and upgrade YAMLs manually at will.

```sh
./examples/sequential-bgd/setup.sh
```

The script accepts optional environment overrides:

```sh
KIND_CLUSTER=desktop IMAGE_TAG=dev OPERATOR_IMAGE_TAG=dev ./examples/sequential-bgd/setup.sh
```

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

The operator and built-in plugin images must also be available in the cluster.
For local development, `setup.sh` builds/loads them and sets the chart image
values. For a published release, use the GHCR defaults from the Helm chart.

Avoid long `--set` chains for the manual install. A checked-in values file is
used below so shell line-continuation whitespace and `zsh` globbing cannot
break the command.

This demo uses local RabbitMQ credentials because it creates its own disposable
broker in `fluidbg-demo`. They are provided to the plugin installation, not to
the BGD. The chart renders local values into Secrets first and injects them via
`secretKeyRef`; production installs should reference existing Secrets instead.

If you run the demo on kind, preload the disposable RabbitMQ infrastructure
image as a single-platform local image before applying `01-base.yaml`. This
keeps the demo independent from Docker Hub pulls inside the kind node:

```sh
KIND_CLUSTER="$(kind get clusters | head -n 1)"
KIND_ARCH="$(kubectl get nodes -o jsonpath='{.items[0].status.nodeInfo.architecture}')"
tmpdir="$(mktemp -d)"
printf 'FROM rabbitmq:4.2-management-alpine\n' > "$tmpdir/Dockerfile"
docker build --platform "linux/$KIND_ARCH" -t rabbitmq:4.2-management-alpine "$tmpdir"
kind load docker-image rabbitmq:4.2-management-alpine --name "$KIND_CLUSTER"
rm -rf "$tmpdir"
```

## Run

If you did not use `setup.sh`, run these commands from the repository root to
install the operator chart into the system namespace and register built-in
plugins in the demo namespace:

```sh
kubectl create namespace fluidbg-demo --dry-run=client -o yaml | kubectl apply -f -

helm upgrade --install fluidbg ./charts/fluidbg-operator \
  --namespace fluidbg-system \
  --create-namespace \
  -f examples/sequential-bgd/operator-values.yaml

kubectl wait --for=jsonpath='{.metadata.name}'=rabbitmq inceptionplugin/rabbitmq -n fluidbg-demo --timeout=60s
kubectl wait --for=jsonpath='{.metadata.name}'=http inceptionplugin/http -n fluidbg-demo --timeout=60s
```

Apply the initial version. This manifest also installs the demo infrastructure
containers into the cluster: the `fluidbg-demo` namespace, the disposable
RabbitMQ broker, the downstream sink, the producer, and the initial
BlueGreenDeployment.

```sh
kubectl apply -f examples/sequential-bgd/01-base.yaml

kubectl wait --for=condition=available deployment/rabbitmq -n fluidbg-demo --timeout=180s
kubectl wait --for=condition=available deployment/order-flow-sink -n fluidbg-demo --timeout=180s
kubectl wait --for=condition=available deployment/order-flow-producer -n fluidbg-demo --timeout=180s

GEN=$(kubectl get bluegreendeployment order-flow -n fluidbg-demo -o jsonpath='{.metadata.generation}')
kubectl wait --for=jsonpath='{.status.observedGeneration}'="$GEN" bluegreendeployment/order-flow -n fluidbg-demo --timeout=180s
kubectl wait --for=jsonpath='{.status.rolloutGeneration}'="$GEN" bluegreendeployment/order-flow -n fluidbg-demo --timeout=180s
kubectl wait --for=jsonpath='{.status.phase}'=Completed bluegreendeployment/order-flow -n fluidbg-demo --timeout=180s
```

Apply the upgraded version:

```sh
kubectl apply -f examples/sequential-bgd/02-upgrade.yaml
GEN=$(kubectl get bluegreendeployment order-flow -n fluidbg-demo -o jsonpath='{.metadata.generation}')
kubectl wait --for=jsonpath='{.status.observedGeneration}'="$GEN" bluegreendeployment/order-flow -n fluidbg-demo --timeout=300s
kubectl wait --for=jsonpath='{.status.rolloutGeneration}'="$GEN" bluegreendeployment/order-flow -n fluidbg-demo --timeout=300s
kubectl wait --for=jsonpath='{.status.phase}'=Completed bluegreendeployment/order-flow -n fluidbg-demo --timeout=300s
```

Watch what happened:

```sh
kubectl get bluegreendeployment order-flow -n fluidbg-demo -o wide
kubectl logs -n fluidbg-demo deploy/order-flow-producer
kubectl logs -n fluidbg-demo deploy/order-flow-sink --tail=300
kubectl logs -n fluidbg-demo -l fluidbg.io/test-name=verifier --tail=100
kubectl get deploy -n fluidbg-demo --show-labels
```

Leave the demo running for a short time after the BGD reaches `Completed`.
That exercises the cleanup path where temporary queue messages have been moved
back and the promoted app is patched back from the HTTP inceptor to the real
sink endpoint:

```sh
sleep 30
kubectl logs -n fluidbg-demo deploy/order-flow-sink --tail=300
kubectl exec -n fluidbg-demo deploy/order-flow-sink -- \
  python -c 'import json, urllib.request; print(json.dumps(json.load(urllib.request.urlopen("http://127.0.0.1:8080/summary")), indent=2, sort_keys=True))'
```

The producer emits incrementing `sequence` values. The sink logs lines like
`HTTP sequence=42` and `OUTPUT sequence=42`, plus a combined output-stream
summary across both prefixes. During a normal run the producer keeps a single
increasing counter, so the no-loss proof is the combined `OUTPUT STREAM OK`
line and `/summary.allOutputMissing == []`.
The base version emits `v1-*` results. The upgraded candidate emits `v2-*`
results. The splitter routes each order to one side, so over time the sink logs
show fewer `v1` lines and more `v2` lines as candidate traffic moves from 10%
to 50% to 100%. The verifier passes a test case only after the separate sink
service has logged both the `v2` HTTP audit call and the `v2` output message for
that candidate sequence. Prefix-specific gaps are expected under a splitter,
because each prefix receives only its routed subset. The combined output stream
is the one that must not have gaps.

The RabbitMQ inception points set semantic `temporaryQueueIdentifier` values so
the generated queues are recognizable without embedding full deployment or base
queue names. Input temporary queues include the token derived from
`incoming-orders`; output temporary queues include the token derived from
`outgoing-results`.

The input inception point uses the RabbitMQ `splitter` role. It registers test
cases only for candidate-routed messages, while current-routed messages keep
flowing through the current app and still end up in the same downstream sink.
The promotion strategy is progressive: every step requires a success rate of
`1.0` for observed candidate cases before the next traffic percentage is
applied. These thresholds use cumulative finalized candidate sample counts, not
per-step deltas. With the demo settings `3`, `8`, `12`, the rollout advances at
3 total finalized candidate cases, then 8 total, then promotes at 12 total.
Because the first two stages route only 10% and 50% of input messages to the
candidate, the producer usually emits more than 12 messages before promotion.
New pending cases do not make the rollout chase a moving tail of producer
traffic, but final cleanup waits until every already-registered case has passed,
failed, or timed out.

The `trafficPercent` value is the candidate-side percentage. The RabbitMQ
splitter hashes the message body and routes it to the candidate/blue queue when
the hash falls into the configured percentage bucket. This is deterministic per
message body, not a pod restart and not a random coin flip per poll. In this
demo the operator starts at 10%, advances to 50%, and finally to 100% after the
cumulative candidate sample threshold for each step passes.

## Cleanup

Use the cleanup helper to remove the demo BGD, wait for finalizer cleanup,
uninstall the Helm release, and delete the demo namespaces:

```sh
examples/sequential-bgd/cleanup.sh
```

By default the cleanup script also removes the FluidBG CRDs because this demo is
intended for a disposable kind cluster. On a shared cluster, keep cluster-scoped
CRDs with:

```sh
DELETE_CRDS=0 examples/sequential-bgd/cleanup.sh
```
