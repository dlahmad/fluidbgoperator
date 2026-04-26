#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
NS="${NS:-fluidbg-test}"

cd "$ROOT_DIR"

cargo run -p fluidbg-operator --bin gen-crds

kubectl apply --server-side --force-conflicts -f "$ROOT_DIR/crds/blue_green_deployment.yaml"
kubectl apply --server-side --force-conflicts -f "$ROOT_DIR/crds/inception_plugin.yaml"
kubectl apply --server-side --force-conflicts -f "$ROOT_DIR/crds/state_store.yaml"

kubectl delete bluegreendeployment order-processor-bg -n "$NS" --ignore-not-found
kubectl delete bluegreendeployment order-processor-bootstrap -n "$NS" --ignore-not-found
kubectl delete bluegreendeployment order-processor-upgrade -n "$NS" --ignore-not-found
kubectl delete statestore memory-store -n "$NS" --ignore-not-found
kubectl delete inceptionplugin rabbitmq -n "$NS" --ignore-not-found
kubectl delete job order-processor-trigger-tests -n "$NS" --ignore-not-found
kubectl delete deployment -n "$NS" -l fluidbg.io/name=order-processor --ignore-not-found
kubectl delete deployment test-container -n "$NS" --ignore-not-found
kubectl delete service test-container -n "$NS" --ignore-not-found
kubectl delete deployment fluidbg-incoming-orders -n "$NS" --ignore-not-found
kubectl delete service fluidbg-incoming-orders-svc -n "$NS" --ignore-not-found
kubectl delete configmap fluidbg-config-incoming-orders -n "$NS" --ignore-not-found
kubectl delete deployment fluidbg-outgoing-results -n "$NS" --ignore-not-found
kubectl delete service fluidbg-outgoing-results-svc -n "$NS" --ignore-not-found
kubectl delete configmap fluidbg-config-outgoing-results -n "$NS" --ignore-not-found
kubectl delete deployment rabbitmq -n fluidbg-system --ignore-not-found
kubectl delete service rabbitmq -n fluidbg-system --ignore-not-found
kubectl delete secret rabbitmq-secret -n fluidbg-system --ignore-not-found
kubectl delete deployment httpbin -n fluidbg-system --ignore-not-found
kubectl delete service httpbin -n fluidbg-system --ignore-not-found
