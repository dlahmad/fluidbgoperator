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

helm uninstall "$RELEASE_NAME" -n "$SYSTEM_NAMESPACE" --ignore-not-found --wait >/dev/null 2>&1 || true

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
