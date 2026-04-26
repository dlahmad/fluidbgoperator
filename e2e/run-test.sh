#!/usr/bin/env bash
set -euo pipefail

NS="${NS:-fluidbg-test}"
NS_SYSTEM="${NS_SYSTEM:-fluidbg-system}"
KIND_CLUSTER="${KIND_CLUSTER:-}"
BUILD_IMAGES="${BUILD_IMAGES:-1}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DEPLOY_DIR="$SCRIPT_DIR/deploy"

RABBITMQ_MGMT_PID=""

cleanup() {
    if [ -n "$RABBITMQ_MGMT_PID" ]; then kill "$RABBITMQ_MGMT_PID" 2>/dev/null || true; fi
}
trap cleanup EXIT

need_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "Missing required command: $1" >&2
        exit 1
    fi
}

apply_namespace() {
    kubectl create namespace "$1" --dry-run=client -o yaml | kubectl apply -f -
}

wait_http() {
    local url="$1"
    local name="$2"
    for i in $(seq 1 30); do
        if curl -sf "$url" >/dev/null 2>&1; then
            echo "$name is ready"
            return 0
        fi
        echo "Waiting for $name... ($i/30)"
        sleep 2
    done
    echo "$name did not become ready" >&2
    return 1
}

wait_deleted() {
    local resource="$1"
    local name="$2"
    local namespace="$3"
    for i in $(seq 1 30); do
        if ! kubectl get "$resource" "$name" -n "$namespace" >/dev/null 2>&1; then
            return 0
        fi
        echo "Waiting for $resource/$name deletion... ($i/30)"
        sleep 1
    done
    echo "$resource/$name was not deleted in namespace $namespace" >&2
    return 1
}

wait_exists() {
    local resource="$1"
    local name="$2"
    local namespace="$3"
    for i in $(seq 1 60); do
        if kubectl get "$resource" "$name" -n "$namespace" >/dev/null 2>&1; then
            return 0
        fi
        echo "Waiting for $resource/$name creation... ($i/60)"
        sleep 1
    done
    echo "$resource/$name was not created in namespace $namespace" >&2
    return 1
}

wait_deployment_label() {
    local deployment="$1"
    local namespace="$2"
    local jsonpath="$3"
    local expected="$4"
    for i in $(seq 1 60); do
        local value
        value="$(kubectl get deployment "$deployment" -n "$namespace" -o "jsonpath=$jsonpath" 2>/dev/null || true)"
        if [ "$value" = "$expected" ]; then
            return 0
        fi
        echo "Waiting for deployment/$deployment label $jsonpath=$expected... ($i/60)"
        sleep 1
    done
    echo "deployment/$deployment did not reach label $jsonpath=$expected in namespace $namespace" >&2
    kubectl get deployment "$deployment" -n "$namespace" -o yaml >&2 || true
    return 1
}

wait_bgd_generated_name() {
    local bgd="$1"
    local namespace="$2"
    for i in $(seq 1 60); do
        local value
        value="$(kubectl get bluegreendeployment "$bgd" -n "$namespace" -o jsonpath='{.status.generatedDeploymentName}' 2>/dev/null || true)"
        if [ -n "$value" ]; then
            printf '%s\n' "$value"
            return 0
        fi
        echo "Waiting for bluegreendeployment/$bgd generated deployment name... ($i/60)" >&2
        sleep 1
    done
    echo "bluegreendeployment/$bgd did not publish status.generatedDeploymentName in namespace $namespace" >&2
    kubectl get bluegreendeployment "$bgd" -n "$namespace" -o yaml >&2 || true
    return 1
}

reset_deployment() {
    local namespace="$1"
    local deployment="$2"
    local app_label="$3"

    kubectl scale deployment "$deployment" -n "$namespace" --replicas=0 >/dev/null
    for i in $(seq 1 60); do
        PODS="$(kubectl get pods -n "$namespace" -l "app=$app_label" --no-headers 2>/dev/null | wc -l | tr -d ' ')"
        if [ "$PODS" = "0" ]; then
            break
        fi
        echo "Waiting for deployment/$deployment pods to stop... ($i/60)"
        sleep 1
    done
    kubectl scale deployment "$deployment" -n "$namespace" --replicas=1 >/dev/null
}

json_number() {
    python3 -c 'import json,sys; data=json.load(sys.stdin); print(data.get(sys.argv[1], 0))' "$1"
}

echo "=== FluidBG E2E Test ==="

need_cmd kubectl
need_cmd curl
need_cmd python3
need_cmd cargo

echo ""
echo "--- Step 0: Regenerate CRDs ---"
cargo run -p fluidbg-operator --bin gen-crds
cp "$ROOT_DIR/crds/blue_green_deployment.yaml" "$DEPLOY_DIR/02-bgd-crd.yaml"
cp "$ROOT_DIR/crds/inception_plugin.yaml" "$DEPLOY_DIR/02-inceptionplugin-crd.yaml"
cp "$ROOT_DIR/crds/state_store.yaml" "$DEPLOY_DIR/02-statestore-crd.yaml"

if [ "$BUILD_IMAGES" = "1" ]; then
    need_cmd docker

    echo ""
    echo "--- Step 0b: Build local images ---"
    docker build -t fluidbg/operator:dev "$ROOT_DIR"
    docker build -f "$ROOT_DIR/plugins/rabbitmq/Dockerfile" -t fluidbg/rabbitmq:dev "$ROOT_DIR"
    docker build -t fluidbg/blue-app:dev "$ROOT_DIR/e2e/blue-app"
    docker build -t fluidbg/green-app:dev "$ROOT_DIR/e2e/green-app"
    docker build -t fluidbg/test-app:dev "$ROOT_DIR/e2e/test-app"

    if command -v kind >/dev/null 2>&1; then
        if [ -z "$KIND_CLUSTER" ]; then
            KIND_CLUSTERS="$(kind get clusters 2>/dev/null || true)"
            if [ "$(printf '%s\n' "$KIND_CLUSTERS" | sed '/^$/d' | wc -l | tr -d ' ')" = "1" ]; then
                KIND_CLUSTER="$(printf '%s\n' "$KIND_CLUSTERS" | sed '/^$/d')"
            else
                KIND_CLUSTER="fluidbg-dev"
            fi
        fi
    fi

    if command -v kind >/dev/null 2>&1 && kind get clusters | grep -qx "$KIND_CLUSTER"; then
        echo ""
        echo "--- Step 0c: Load images into kind cluster '$KIND_CLUSTER' ---"
        kind load docker-image fluidbg/operator:dev --name "$KIND_CLUSTER"
        kind load docker-image fluidbg/rabbitmq:dev --name "$KIND_CLUSTER"
        kind load docker-image fluidbg/blue-app:dev --name "$KIND_CLUSTER"
        kind load docker-image fluidbg/green-app:dev --name "$KIND_CLUSTER"
        kind load docker-image fluidbg/test-app:dev --name "$KIND_CLUSTER"
    fi
fi

echo ""
echo "--- Step 1: Deploy infrastructure ---"
apply_namespace "$NS_SYSTEM"
apply_namespace "$NS"
kubectl apply -f "$DEPLOY_DIR/01-httpbin.yaml"
kubectl apply -f "$DEPLOY_DIR/01-rabbitmq.yaml"
reset_deployment "$NS_SYSTEM" rabbitmq rabbitmq
kubectl rollout status deployment/rabbitmq -n "$NS_SYSTEM" --timeout=120s
kubectl rollout status deployment/httpbin -n "$NS_SYSTEM" --timeout=120s
echo "Waiting for RabbitMQ readiness (extra 15s)..."
sleep 15

echo ""
echo "--- Step 2: Deploy CRDs ---"
kubectl apply --server-side --force-conflicts -f "$DEPLOY_DIR/02-bgd-crd.yaml"
kubectl apply --server-side --force-conflicts -f "$DEPLOY_DIR/02-inceptionplugin-crd.yaml"
kubectl apply --server-side --force-conflicts -f "$DEPLOY_DIR/02-statestore-crd.yaml"
kubectl delete bluegreendeployment order-processor-bg -n "$NS" --ignore-not-found
kubectl delete bluegreendeployment order-processor-bootstrap -n "$NS" --ignore-not-found
kubectl delete bluegreendeployment order-processor-upgrade -n "$NS" --ignore-not-found
kubectl delete statestore memory-store -n "$NS" --ignore-not-found
kubectl delete inceptionplugin rabbitmq -n "$NS" --ignore-not-found
kubectl delete deployment -n "$NS" -l fluidbg.io/name=order-processor --ignore-not-found
kubectl delete deployment test-container -n "$NS" --ignore-not-found
kubectl delete service test-container -n "$NS" --ignore-not-found
kubectl delete deployment fluidbg-incoming-orders -n "$NS" --ignore-not-found
kubectl delete service fluidbg-incoming-orders-svc -n "$NS" --ignore-not-found
kubectl delete configmap fluidbg-config-incoming-orders -n "$NS" --ignore-not-found
kubectl delete deployment fluidbg-outgoing-results -n "$NS" --ignore-not-found
kubectl delete service fluidbg-outgoing-results-svc -n "$NS" --ignore-not-found
kubectl delete configmap fluidbg-config-outgoing-results -n "$NS" --ignore-not-found
wait_deleted bluegreendeployment order-processor-bg "$NS"
wait_deleted bluegreendeployment order-processor-bootstrap "$NS"
wait_deleted bluegreendeployment order-processor-upgrade "$NS"
wait_deleted statestore memory-store "$NS"
wait_deleted inceptionplugin rabbitmq "$NS"
wait_deleted deployment test-container "$NS"
wait_deleted service test-container "$NS"
wait_deleted deployment fluidbg-incoming-orders "$NS"
wait_deleted service fluidbg-incoming-orders-svc "$NS"
wait_deleted configmap fluidbg-config-incoming-orders "$NS"
wait_deleted deployment fluidbg-outgoing-results "$NS"
wait_deleted service fluidbg-outgoing-results-svc "$NS"
wait_deleted configmap fluidbg-config-outgoing-results "$NS"

echo ""
echo "--- Step 3: Deploy operator ---"
kubectl apply -f "$DEPLOY_DIR/03-operator.yaml"
reset_deployment "$NS_SYSTEM" fluidbg-operator fluidbg-operator
kubectl rollout status deployment/fluidbg-operator -n "$NS_SYSTEM" --timeout=120s

echo ""
echo "--- Step 4: Bootstrap initial green from empty cluster ---"
kubectl apply -f "$DEPLOY_DIR/05-bootstrap-bgd.yaml"
BOOTSTRAP_DEPLOYMENT="$(wait_bgd_generated_name order-processor-bootstrap "$NS")"
for i in $(seq 1 30); do
    BOOTSTRAP_PHASE="$(kubectl get bluegreendeployment order-processor-bootstrap -n "$NS" -o jsonpath='{.status.phase}' 2>/dev/null || true)"
    echo "  Bootstrap BGD phase: ${BOOTSTRAP_PHASE:-<none>} ($i/30)"
    if [ "$BOOTSTRAP_PHASE" = "Completed" ]; then
        break
    fi
    sleep 2
done
BOOTSTRAP_PHASE="$(kubectl get bluegreendeployment order-processor-bootstrap -n "$NS" -o jsonpath='{.status.phase}')"
if [ "$BOOTSTRAP_PHASE" != "Completed" ]; then
    echo "Expected bootstrap BGD phase Completed, got '$BOOTSTRAP_PHASE'" >&2
    kubectl get bluegreendeployment order-processor-bootstrap -n "$NS" -o yaml >&2
    exit 1
fi
wait_exists deployment "$BOOTSTRAP_DEPLOYMENT" "$NS"
kubectl rollout status deployment/"$BOOTSTRAP_DEPLOYMENT" -n "$NS" --timeout=120s
wait_deployment_label "$BOOTSTRAP_DEPLOYMENT" "$NS" '{.metadata.labels.fluidbg\.io/green}' true

echo ""
echo "--- Step 5: Deploy upgrade BlueGreenDeployment CR ---"
kubectl apply -f "$DEPLOY_DIR/06-upgrade-bgd.yaml"
sleep 5
UPGRADE_DEPLOYMENT="$(wait_bgd_generated_name order-processor-upgrade "$NS")"
wait_exists deployment "$UPGRADE_DEPLOYMENT" "$NS"
wait_exists deployment test-container "$NS"
wait_exists deployment fluidbg-incoming-orders "$NS"
wait_exists deployment fluidbg-outgoing-results "$NS"
kubectl rollout status deployment/"$BOOTSTRAP_DEPLOYMENT" -n "$NS" --timeout=120s
kubectl rollout status deployment/"$UPGRADE_DEPLOYMENT" -n "$NS" --timeout=120s
kubectl rollout status deployment/test-container -n "$NS" --timeout=120s
kubectl rollout status deployment/fluidbg-incoming-orders -n "$NS" --timeout=120s
kubectl rollout status deployment/fluidbg-outgoing-results -n "$NS" --timeout=120s

echo ""
echo "--- Step 6: Check BGD status ---"
kubectl get bluegreendeployment order-processor-bootstrap -n "$NS" -o yaml
kubectl get bluegreendeployment order-processor-upgrade -n "$NS" -o yaml

echo ""
echo "--- Step 7: Port-forward RabbitMQ management API ---"
kubectl port-forward svc/rabbitmq 15672:15672 -n "$NS_SYSTEM" >/tmp/fluidbg-rabbitmq-port-forward.log 2>&1 &
RABBITMQ_MGMT_PID=$!
sleep 3
wait_http http://localhost:15672 rabbitmq-management

echo ""
echo "--- Step 8: Publish input messages to RabbitMQ ---"
for i in 1 2 3 4 5; do
    echo "Publishing order-$i..."
    curl -sf -u fluidbg:fluidbg \
        -H "Content-Type: application/json" \
        -X POST http://localhost:15672/api/exchanges/%2F/amq.default/publish \
        -d "{\"properties\":{},\"routing_key\":\"orders\",\"payload\":\"{\\\"orderId\\\":\\\"order-$i\\\",\\\"type\\\":\\\"order\\\",\\\"action\\\":\\\"process\\\"}\",\"payload_encoding\":\"string\"}" >/dev/null
    sleep 1
done

echo ""
echo "--- Step 9: Wait for operator-observed test cases ---"
OBSERVED_COUNT=0
PASSED_COUNT=0
PENDING_COUNT=0
for i in $(seq 1 60); do
    STATUS_JSON="$(kubectl get bluegreendeployment order-processor-upgrade -n "$NS" -o json)"
    OBSERVED_COUNT="$(printf '%s' "$STATUS_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status", {}).get("testCasesObserved", 0))')"
    PASSED_COUNT="$(printf '%s' "$STATUS_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status", {}).get("testCasesPassed", 0))')"
    PENDING_COUNT="$(printf '%s' "$STATUS_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status", {}).get("testCasesPending", 0))')"
    PHASE="$(printf '%s' "$STATUS_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status", {}).get("phase", ""))')"
    TRACKED_COUNT="$((OBSERVED_COUNT + PENDING_COUNT))"
    echo "  Test cases tracked(observed+pending)/passed: $TRACKED_COUNT($OBSERVED_COUNT+$PENDING_COUNT)/$PASSED_COUNT phase=${PHASE:-<none>} ($i/60)"
    if [ "$TRACKED_COUNT" -ge 5 ] && [ "$PASSED_COUNT" -ge 3 ]; then
        break
    fi
    if [ "$PHASE" = "RolledBack" ]; then
        echo "BGD rolled back while waiting for observed test cases" >&2
        kubectl get bluegreendeployment order-processor-upgrade -n "$NS" -o yaml >&2
        exit 1
    fi
    sleep 5
done

if [ "$((OBSERVED_COUNT + PENDING_COUNT))" -lt 5 ] || [ "$PASSED_COUNT" -lt 3 ]; then
    echo "Expected at least 5 tracked and 3 passed test cases; got observed=$OBSERVED_COUNT pending=$PENDING_COUNT passed=$PASSED_COUNT" >&2
    kubectl get bluegreendeployment order-processor-upgrade -n "$NS" -o yaml >&2
    exit 1
fi

echo ""
echo "--- Step 10: Wait for promotion ---"
for i in $(seq 1 30); do
    STATUS_JSON="$(kubectl get bluegreendeployment order-processor-upgrade -n "$NS" -o json)"
    echo "$STATUS_JSON" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin).get("status", {}), indent=2))'
    PHASE="$(printf '%s' "$STATUS_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status", {}).get("phase", ""))')"
    echo "  BGD phase: ${PHASE:-<none>} ($i/30)"
    if [ "$PHASE" = "Completed" ]; then
        break
    fi
    if [ "$PHASE" = "RolledBack" ]; then
        echo "BGD rolled back" >&2
        exit 1
    fi
    sleep 5
done

PHASE="$(kubectl get bluegreendeployment order-processor-upgrade -n "$NS" -o jsonpath='{.status.phase}')"
if [ "$PHASE" != "Completed" ]; then
    echo "Expected BGD phase Completed, got '$PHASE'" >&2
    kubectl get bluegreendeployment order-processor-upgrade -n "$NS" -o yaml
    exit 1
fi

echo ""
echo "--- Step 10b: Verify previous green cleanup and promoted labels ---"
wait_deleted deployment "$BOOTSTRAP_DEPLOYMENT" "$NS"
wait_deleted deployment test-container "$NS"
wait_deleted service test-container "$NS"
PROMOTED_GREEN="$(kubectl get deployment "$UPGRADE_DEPLOYMENT" -n "$NS" -o jsonpath='{.metadata.labels.fluidbg\.io/green}')"
if [ "$PROMOTED_GREEN" != "true" ]; then
    echo "Expected promoted deployment to have fluidbg.io/green=true, got '$PROMOTED_GREEN'" >&2
    kubectl get deployment "$UPGRADE_DEPLOYMENT" -n "$NS" -o yaml
    exit 1
fi

echo ""
echo "--- Step 11: Final BGD status ---"
kubectl get bluegreendeployment order-processor-upgrade -n "$NS" -o jsonpath='{.status}' | python3 -m json.tool

echo ""
echo "--- Step 12: Check all pods ---"
kubectl get pods -n "$NS"
kubectl get pods -n "$NS_SYSTEM"

echo ""
echo "=== E2E Test Complete ==="
