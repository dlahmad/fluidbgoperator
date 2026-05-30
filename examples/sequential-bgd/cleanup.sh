#!/usr/bin/env bash
set -euo pipefail

NAMESPACE="${NAMESPACE:-fluidbg-demo}"
SYSTEM_NAMESPACE="${SYSTEM_NAMESPACE:-fluidbg-system}"
RELEASE_NAME="${RELEASE_NAME:-fluidbg}"
DELETE_CRDS="${DELETE_CRDS:-1}"

echo "Cleaning FluidBG sequential demo"
echo "  namespace:        $NAMESPACE"
echo "  system namespace: $SYSTEM_NAMESPACE"
echo "  release:          $RELEASE_NAME"
echo "  delete CRDs:      $DELETE_CRDS"

if kubectl get bluegreendeployment order-flow -n "$NAMESPACE" >/dev/null 2>&1; then
    kubectl delete bluegreendeployment order-flow -n "$NAMESPACE" --wait=true --timeout=180s
else
    kubectl delete bluegreendeployment order-flow -n "$NAMESPACE" --ignore-not-found
fi

if kubectl get bluegreendeployment order-flow-nats -n "$NAMESPACE" >/dev/null 2>&1; then
    kubectl delete bluegreendeployment order-flow-nats -n "$NAMESPACE" --wait=true --timeout=180s
else
    kubectl delete bluegreendeployment order-flow-nats -n "$NAMESPACE" --ignore-not-found
fi

helm uninstall "$RELEASE_NAME" -n "$SYSTEM_NAMESPACE" --ignore-not-found --wait >/dev/null 2>&1 || true

if kubectl get crd bluegreendeployments.fluidbg.io >/dev/null 2>&1; then
    while read -r bgd_namespace bgd_name; do
        [ -n "$bgd_namespace" ] || continue
        kubectl patch bluegreendeployment "$bgd_name" -n "$bgd_namespace" \
            --type=merge -p '{"metadata":{"finalizers":[]}}' >/dev/null 2>&1 || true
        kubectl delete bluegreendeployment "$bgd_name" -n "$bgd_namespace" \
            --ignore-not-found --wait=false >/dev/null 2>&1 || true
    done < <(kubectl get bluegreendeployment -A --no-headers \
        -o custom-columns='NAMESPACE:.metadata.namespace,NAME:.metadata.name' 2>/dev/null || true)

    for _ in $(seq 1 90); do
        if [ -z "$(kubectl get bluegreendeployment -A --no-headers 2>/dev/null || true)" ]; then
            break
        fi
        sleep 1
    done
fi

kubectl delete namespace "$NAMESPACE" --ignore-not-found --wait=true --timeout=180s
kubectl delete namespace "$SYSTEM_NAMESPACE" --ignore-not-found --wait=true --timeout=180s

if [ "$DELETE_CRDS" = "1" ]; then
    kubectl delete crd \
        bluegreendeployments.fluidbg.io \
        inceptionplugins.fluidbg.io \
        --ignore-not-found \
        --wait=true \
        --timeout=180s
fi

echo "Cleanup complete."
