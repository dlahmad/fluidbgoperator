# Sequential Order Flow Demo

This example shows one blue-green rollout with live queue traffic:

- a producer publishes orders to RabbitMQ
- the application under test consumes each order
- the application calls an HTTP audit endpoint
- the application publishes an output message
- one normal downstream demo sink service receives and logs the HTTP audit calls and final output messages
- one verifier test container asks the sink whether the candidate effects arrived
- the operator promotes only after the sink has seen both effects for candidate traffic

## Images

The example YAML uses published GHCR images:

- `ghcr.io/dlahmad/fluidbg-example-order-app:latest`
- `ghcr.io/dlahmad/fluidbg-example-producer:latest`
- `ghcr.io/dlahmad/fluidbg-example-sink:latest`
- `ghcr.io/dlahmad/fluidbg-example-verifier:latest`

For local development, build and load replacement images with the same tags:

```sh
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

The sink is intentionally not the test container. It simulates a downstream
service that would already exist in a real system: it consumes the stable
`results` queue and exposes a stable HTTP audit endpoint. The verifier is a
separate operator-created test container that only queries the sink API to
decide whether candidate traffic is safe to promote.

The operator and built-in plugin images must also be available in the cluster.
For local development, build/load them with the repository scripts. For a
published release, use the GHCR defaults in the Helm chart. If you are testing
local `:dev` operator/plugin images, set the chart image values explicitly:

```sh
helm upgrade --install fluidbg charts/fluidbg-operator \
  --namespace fluidbg-system \
  --create-namespace \
  --set operator.image.repository=fluidbg/fbg-operator \
  --set operator.image.tag=dev \
  --set operator.image.pullPolicy=Never \
  --set builtinPlugins.http.image.repository=fluidbg/fbg-plugin-http \
  --set builtinPlugins.http.image.tag=dev \
  --set builtinPlugins.rabbitmq.image.repository=fluidbg/fbg-plugin-rabbitmq \
  --set builtinPlugins.rabbitmq.image.tag=dev \
  --set builtinPlugins.rabbitmq.manager.enabled=true \
  --set builtinPlugins.rabbitmq.manager.amqpUrl='amqp://fluidbg:fluidbg@rabbitmq.fluidbg-demo:5672/%2f' \
  --set builtinPlugins.rabbitmq.manager.managementUrl='http://rabbitmq.fluidbg-demo:15672' \
  --set builtinPlugins.rabbitmq.manager.managementUsername=fluidbg \
  --set builtinPlugins.rabbitmq.manager.managementPassword=fluidbg \
  --set builtinPlugins.rabbitmq.manager.managementVhost='/' \
  --set operator.auth.createSigningSecret=true \
  --set operator.auth.signingSecretName=fluidbg-operator-auth \
  --set operator.auth.signingSecretValue=dev-signing-key-change-me \
  --set 'builtinPlugins.namespaces[0]=fluidbg-demo'
```

This demo uses local RabbitMQ credentials because it creates its own disposable
broker in `fluidbg-demo`. They are provided to the plugin installation, not to
the BGD. The chart renders local values into Secrets first and injects them via
`secretKeyRef`; production installs should reference existing Secrets instead.

## Run

Install the operator chart into the system namespace and register built-in
plugins in the demo namespace:

```sh
kubectl create namespace fluidbg-demo --dry-run=client -o yaml | kubectl apply -f -

helm upgrade --install fluidbg charts/fluidbg-operator \
  --namespace fluidbg-system \
  --create-namespace \
  --set operator.auth.createSigningSecret=true \
  --set operator.auth.signingSecretName=fluidbg-operator-auth \
  --set operator.auth.signingSecretValue=dev-signing-key-change-me \
  --set 'builtinPlugins.namespaces[0]=fluidbg-demo'
```

Apply the initial version:

```sh
kubectl apply -f examples/sequential-bgd/01-base.yaml
kubectl wait --for=jsonpath='{.status.phase}'=Completed bluegreendeployment/order-flow -n fluidbg-demo --timeout=180s
```

Apply the upgraded version:

```sh
kubectl apply -f examples/sequential-bgd/02-upgrade.yaml
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

The producer emits incrementing `sequence` values. The sink logs lines like
`HTTP sequence=42` and `OUTPUT sequence=42`, plus a current missing-sequence
summary for the HTTP stream, output stream, and completed HTTP+output pairs.
During a normal run the producer keeps a single increasing counter, so the user
can inspect the sink logs during and after promotion and see whether any number
disappeared.
The base version emits `v1-*` results. The upgraded candidate emits `v2-*`
results. The verifier passes a test case only after the separate sink service
has logged both the HTTP audit call and the output message for that `v2`
candidate sequence.

## Cleanup

Delete the `BlueGreenDeployment` while the operator is still installed, and wait
until the CR disappears so the finalizer can clean inceptors, verifier
resources, and plugin state before Helm removes the operator:

```sh
kubectl delete bluegreendeployment order-flow -n fluidbg-demo --ignore-not-found
kubectl wait --for=delete bluegreendeployment/order-flow -n fluidbg-demo --timeout=180s
helm uninstall fluidbg -n fluidbg-system --ignore-not-found --wait
kubectl delete namespace fluidbg-demo --ignore-not-found
```
