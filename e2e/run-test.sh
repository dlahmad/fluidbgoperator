#!/usr/bin/env bash
set -euo pipefail

NS="${NS:-fluidbg-test}"
NS_SYSTEM="${NS_SYSTEM:-fluidbg-system}"
KIND_CLUSTER="${KIND_CLUSTER:-}"
BUILD_IMAGES="${BUILD_IMAGES:-1}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DEPLOY_DIR="$SCRIPT_DIR/deploy"

TEST_CONTAINER_PID=""
OPERATOR_PID=""

cleanup() {
    if [ -n "$TEST_CONTAINER_PID" ]; then kill "$TEST_CONTAINER_PID" 2>/dev/null || true; fi
    if [ -n "$OPERATOR_PID" ]; then kill "$OPERATOR_PID" 2>/dev/null || true; fi
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
kubectl delete statestore memory-store -n "$NS" --ignore-not-found
kubectl delete inceptionplugin rabbitmq -n "$NS" --ignore-not-found
kubectl delete deployment order-processor-blue -n "$NS" --ignore-not-found
kubectl delete deployment test-container -n "$NS" --ignore-not-found
kubectl delete service test-container -n "$NS" --ignore-not-found
kubectl delete deployment fluidbg-incoming-orders -n "$NS" --ignore-not-found
kubectl delete service fluidbg-incoming-orders-svc -n "$NS" --ignore-not-found
kubectl delete configmap fluidbg-config-incoming-orders -n "$NS" --ignore-not-found
kubectl delete deployment fluidbg-outgoing-results -n "$NS" --ignore-not-found
kubectl delete service fluidbg-outgoing-results-svc -n "$NS" --ignore-not-found
kubectl delete configmap fluidbg-config-outgoing-results -n "$NS" --ignore-not-found
wait_deleted bluegreendeployment order-processor-bg "$NS"
wait_deleted statestore memory-store "$NS"
wait_deleted inceptionplugin rabbitmq "$NS"
wait_deleted deployment order-processor-blue "$NS"
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
echo "--- Step 4: Deploy sample app ---"
kubectl apply -f "$DEPLOY_DIR/04-sample-app.yaml"
reset_deployment "$NS" order-processor-green order-processor-green
kubectl rollout status deployment/order-processor-green -n "$NS" --timeout=120s

echo ""
echo "--- Step 5: Deploy BlueGreenDeployment CR ---"
kubectl apply -f "$DEPLOY_DIR/05-bgd.yaml"
sleep 5
wait_exists deployment order-processor-blue "$NS"
wait_exists deployment test-container "$NS"
wait_exists deployment fluidbg-incoming-orders "$NS"
wait_exists deployment fluidbg-outgoing-results "$NS"
kubectl rollout status deployment/order-processor-green -n "$NS" --timeout=120s
kubectl rollout status deployment/order-processor-blue -n "$NS" --timeout=120s
kubectl rollout status deployment/test-container -n "$NS" --timeout=120s
kubectl rollout status deployment/fluidbg-incoming-orders -n "$NS" --timeout=120s
kubectl rollout status deployment/fluidbg-outgoing-results -n "$NS" --timeout=120s

echo ""
echo "--- Step 6: Check BGD status ---"
kubectl get bluegreendeployments.fluidbg.io -n "$NS" -o yaml

echo ""
echo "--- Step 7: Port-forward test-container and operator ---"
kubectl port-forward svc/test-container 18080:8080 -n "$NS" >/tmp/fluidbg-test-container-port-forward.log 2>&1 &
TEST_CONTAINER_PID=$!
kubectl port-forward svc/fluidbg-operator 18090:8090 -n "$NS_SYSTEM" >/tmp/fluidbg-operator-port-forward.log 2>&1 &
OPERATOR_PID=$!
sleep 3
wait_http http://localhost:18080/health test-container
wait_http http://localhost:18090/health operator

echo ""
echo "--- Step 8: Register cases with operator ---"
for i in 1 2 3 4 5; do
    curl -sf -X POST http://localhost:18090/cases \
        -H "Content-Type: application/json" \
        -d "{
            \"test_id\": \"order-$i\",
            \"blue_green_ref\": \"order-processor-bg\",
            \"inception_point\": \"incoming-orders\",
            \"timeout_seconds\": 120
        }" >/dev/null
    echo "registered order-$i"
done

echo ""
echo "--- Step 9: Send trigger messages ---"
for i in 1 2 3 4 5; do
    echo "Triggering message $i..."
    curl -sf -X POST http://localhost:18080/trigger \
        -H "Content-Type: application/json" \
        -d "{\"testId\": \"order-$i\"}" >/dev/null
    sleep 1
done

echo ""
echo "--- Step 10: Wait for sample processing ---"
PASSED_COUNT=0
for i in $(seq 1 60); do
    CASES_JSON="$(curl -sf http://localhost:18080/cases)"
    PASSED_COUNT="$(printf '%s' "$CASES_JSON" | python3 -c '
import json, sys
data = json.load(sys.stdin)
print(sum(1 for v in data.values() if v.get("status") == "passed"))
')"
    TOTAL_COUNT="$(printf '%s' "$CASES_JSON" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))')"
    echo "  Cases passed: $PASSED_COUNT/$TOTAL_COUNT ($i/60)"
    if [ "$PASSED_COUNT" -ge 3 ]; then
        break
    fi
    sleep 5
done

if [ "$PASSED_COUNT" -lt 3 ]; then
    echo "Expected at least 3 passed cases before promotion; got $PASSED_COUNT" >&2
    curl -sf http://localhost:18080/cases | python3 -m json.tool
    exit 1
fi

echo ""
echo "--- Step 11: Send verdicts to operator ---"
for i in 1 2 3 4 5; do
    RESULT_JSON="$(curl -sf "http://localhost:18080/result/order-$i")"
    PASSED="$(printf '%s' "$RESULT_JSON" | python3 -c 'import json,sys; print("true" if json.load(sys.stdin).get("passed") is True else "false")')"
    curl -sf -X POST http://localhost:18090/verdict \
        -H "Content-Type: application/json" \
        -d "{\"test_id\": \"order-$i\", \"passed\": $PASSED}" >/dev/null
    echo "operator verdict order-$i=$PASSED"
done

echo ""
echo "--- Step 12: Wait for promotion ---"
for i in $(seq 1 30); do
    COUNTS_JSON="$(curl -sf http://localhost:18090/counts/order-processor-bg)"
    echo "$COUNTS_JSON" | python3 -m json.tool
    PHASE="$(kubectl get bluegreendeployment order-processor-bg -n "$NS" -o jsonpath='{.status.phase}' 2>/dev/null || true)"
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

PHASE="$(kubectl get bluegreendeployment order-processor-bg -n "$NS" -o jsonpath='{.status.phase}')"
if [ "$PHASE" != "Completed" ]; then
    echo "Expected BGD phase Completed, got '$PHASE'" >&2
    kubectl get bluegreendeployment order-processor-bg -n "$NS" -o yaml
    exit 1
fi

echo ""
echo "--- Step 12b: Verify previous green cleanup and promoted labels ---"
wait_deleted deployment order-processor-green "$NS"
wait_deleted deployment test-container "$NS"
wait_deleted service test-container "$NS"
PROMOTED_ROLE="$(kubectl get deployment order-processor-blue -n "$NS" -o jsonpath='{.metadata.labels.fluidbg\.io/role}')"
if [ "$PROMOTED_ROLE" != "green" ]; then
    echo "Expected promoted blue deployment to have fluidbg.io/role=green, got '$PROMOTED_ROLE'" >&2
    kubectl get deployment order-processor-blue -n "$NS" -o yaml
    exit 1
fi

echo ""
echo "--- Step 13: Final BGD status ---"
kubectl get bluegreendeployment order-processor-bg -n "$NS" -o jsonpath='{.status}' | python3 -m json.tool

echo ""
echo "--- Step 14: Check all pods ---"
kubectl get pods -n "$NS"
kubectl get pods -n "$NS_SYSTEM"

echo ""
echo "=== E2E Test Complete ==="
