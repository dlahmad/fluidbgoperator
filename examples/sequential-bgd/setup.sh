#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

KIND_CLUSTER="${KIND_CLUSTER:-$(kind get clusters | head -n 1)}"
NAMESPACE="${NAMESPACE:-fluidbg-demo}"
SYSTEM_NAMESPACE="${SYSTEM_NAMESPACE:-fluidbg-system}"
IMAGE_REGISTRY="${IMAGE_REGISTRY:-fluidbg}"
IMAGE_TAG="${IMAGE_TAG:-dev}"
OPERATOR_IMAGE_REGISTRY="${OPERATOR_IMAGE_REGISTRY:-fluidbg}"
OPERATOR_IMAGE_TAG="${OPERATOR_IMAGE_TAG:-dev}"
BUILD_EXAMPLE_IMAGES="${BUILD_EXAMPLE_IMAGES:-1}"
BUILD_OPERATOR_IMAGES="${BUILD_OPERATOR_IMAGES:-1}"
RABBITMQ_IMAGE="${RABBITMQ_IMAGE:-rabbitmq:4.2-management-alpine}"
if [ -z "${OPERATOR_IMAGE_PULL_POLICY:-}" ]; then
    if [ "$BUILD_OPERATOR_IMAGES" = "1" ] || [ "$OPERATOR_IMAGE_REGISTRY" != "ghcr.io/dlahmad" ]; then
        OPERATOR_IMAGE_PULL_POLICY="Never"
    else
        OPERATOR_IMAGE_PULL_POLICY="IfNotPresent"
    fi
fi

if [ -z "$KIND_CLUSTER" ]; then
    echo "No kind cluster found. Set KIND_CLUSTER or create a kind cluster first." >&2
    exit 1
fi

KIND_ARCH="$(kubectl get nodes -o jsonpath='{.items[0].status.nodeInfo.architecture}')"
PLATFORM="linux/$KIND_ARCH"

echo "Preparing FluidBG sequential demo"
echo "  kind cluster:      $KIND_CLUSTER"
echo "  namespace:         $NAMESPACE"
echo "  system namespace:  $SYSTEM_NAMESPACE"
echo "  platform:          $PLATFORM"
echo "  example images:    $IMAGE_REGISTRY/*:$IMAGE_TAG"
echo "  operator images:   $OPERATOR_IMAGE_REGISTRY/*:$OPERATOR_IMAGE_TAG"
echo "  pull policy:       $OPERATOR_IMAGE_PULL_POLICY"

if [ "$BUILD_EXAMPLE_IMAGES" = "1" ]; then
    "$ROOT_DIR/scripts/build-example-images.sh" --registry "$IMAGE_REGISTRY" --tag "$IMAGE_TAG"
fi

for image in \
    "$IMAGE_REGISTRY/fluidbg-example-order-app:$IMAGE_TAG" \
    "$IMAGE_REGISTRY/fluidbg-example-producer:$IMAGE_TAG" \
    "$IMAGE_REGISTRY/fluidbg-example-sink:$IMAGE_TAG" \
    "$IMAGE_REGISTRY/fluidbg-example-verifier:$IMAGE_TAG"; do
    kind load docker-image "$image" --name "$KIND_CLUSTER"
done

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT
printf 'FROM %s\n' "$RABBITMQ_IMAGE" > "$tmpdir/Dockerfile"
docker build --platform "$PLATFORM" -t "$RABBITMQ_IMAGE" "$tmpdir"
kind load docker-image "$RABBITMQ_IMAGE" --name "$KIND_CLUSTER"

if [ "$BUILD_OPERATOR_IMAGES" = "1" ]; then
    "$ROOT_DIR/scripts/build-linux-binaries.sh" --arch "$KIND_ARCH"
    "$ROOT_DIR/scripts/build-images.sh" \
        --registry "$OPERATOR_IMAGE_REGISTRY" \
        --tag "$OPERATOR_IMAGE_TAG" \
        --platform "$PLATFORM"

    for image in \
        "$OPERATOR_IMAGE_REGISTRY/fbg-operator:$OPERATOR_IMAGE_TAG" \
        "$OPERATOR_IMAGE_REGISTRY/fbg-plugin-http:$OPERATOR_IMAGE_TAG" \
        "$OPERATOR_IMAGE_REGISTRY/fbg-plugin-rabbitmq:$OPERATOR_IMAGE_TAG" \
        "$OPERATOR_IMAGE_REGISTRY/fbg-plugin-azure-servicebus:$OPERATOR_IMAGE_TAG"; do
        kind load docker-image "$image" --name "$KIND_CLUSTER"
    done
fi

kubectl create namespace "$NAMESPACE" --dry-run=client -o yaml | kubectl apply -f -

helm_args=(
    upgrade --install fluidbg "$ROOT_DIR/charts/fluidbg-operator"
    --namespace "$SYSTEM_NAMESPACE"
    --create-namespace
    -f "$SCRIPT_DIR/operator-values.yaml"
    --set "global.imageTag=$OPERATOR_IMAGE_TAG"
    --set "operator.image.repository=$OPERATOR_IMAGE_REGISTRY/fbg-operator"
    --set "operator.image.pullPolicy=$OPERATOR_IMAGE_PULL_POLICY"
    --set "builtinPlugins.http.image.repository=$OPERATOR_IMAGE_REGISTRY/fbg-plugin-http"
    --set "builtinPlugins.rabbitmq.image.repository=$OPERATOR_IMAGE_REGISTRY/fbg-plugin-rabbitmq"
    --set "builtinPlugins.azureServiceBus.image.repository=$OPERATOR_IMAGE_REGISTRY/fbg-plugin-azure-servicebus"
    --set "builtinPlugins.rabbitmq.manager.amqpUrl=amqp://fluidbg:fluidbg@rabbitmq.$NAMESPACE:5672/%2f"
    --set "builtinPlugins.rabbitmq.manager.managementUrl=http://rabbitmq.$NAMESPACE:15672"
    --set "builtinPlugins.namespaces[0]=$NAMESPACE"
)

helm "${helm_args[@]}"

kubectl rollout status "deploy/fluidbg-fluidbg-operator" -n "$SYSTEM_NAMESPACE" --timeout=180s
kubectl rollout status "deploy/fluidbg-rabbitmq-manager" -n "$SYSTEM_NAMESPACE" --timeout=180s
kubectl wait --for=jsonpath='{.metadata.name}'=rabbitmq inceptionplugin/rabbitmq -n "$NAMESPACE" --timeout=60s
kubectl wait --for=jsonpath='{.metadata.name}'=http inceptionplugin/http -n "$NAMESPACE" --timeout=60s

cat <<EOF

Setup complete.

Apply the demo resources when ready:
  kubectl apply -f examples/sequential-bgd/01-base.yaml

Then apply the upgrade:
  kubectl apply -f examples/sequential-bgd/02-upgrade.yaml
EOF
