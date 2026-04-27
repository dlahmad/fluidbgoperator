#!/usr/bin/env bash
set -euo pipefail

NS="${NS:-fluidbg-test}"
NS_SYSTEM="${NS_SYSTEM:-fluidbg-system}"
KIND_CLUSTER="${KIND_CLUSTER:-}"
BUILD_IMAGES="${BUILD_IMAGES:-1}"
E2E_STATE_STORE="${E2E_STATE_STORE:-memory}"
OPERATOR_REPLICAS="${OPERATOR_REPLICAS:-1}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DEPLOY_DIR="$SCRIPT_DIR/deploy"

RABBITMQ_MGMT_PID=""
RABBITMQ_MGMT_PORT="${RABBITMQ_MGMT_PORT:-$((25000 + RANDOM % 20000))}"
RABBITMQ_MGMT_URL="http://localhost:${RABBITMQ_MGMT_PORT}"

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

target_arch() {
    local arch
    arch="${TARGET_ARCH:-}"
    if [ -z "$arch" ]; then
        arch="$(kubectl get nodes -o jsonpath='{.items[0].status.nodeInfo.architecture}' 2>/dev/null || true)"
    fi
    if [ -z "$arch" ]; then
        arch="amd64"
    fi
    printf '%s\n' "$arch"
}

build_linux_rust_binaries() {
    local arch="$1"
    "$ROOT_DIR/scripts/build-linux-binaries.sh" --arch "$arch"
}

prefetch_linux_rust_dependencies() {
    local arch="$1"
    local platform="linux/$arch"
    mkdir -p "$ROOT_DIR/.docker-cargo-home/registry" "$ROOT_DIR/.docker-cargo-home/git"
    docker run --rm --platform "$platform" \
        -v "$ROOT_DIR":/work \
        -v "$ROOT_DIR/.docker-cargo-home":/cargo-home \
        -e CARGO_HOME=/cargo-home \
        -e CARGO_HTTP_TIMEOUT=600 \
        -e CARGO_NET_RETRY=10 \
        -e CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse \
        -w /work \
        rust:bookworm \
        bash -lc '
            set -euo pipefail
            export PATH=/usr/local/cargo/bin:$PATH
            cargo fetch --locked
        '
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

ensure_rabbitmq_management_port_forward() {
    if [ -n "$RABBITMQ_MGMT_PID" ] && kill -0 "$RABBITMQ_MGMT_PID" 2>/dev/null; then
        if curl -sf "$RABBITMQ_MGMT_URL" >/dev/null 2>&1; then
            return 0
        fi
        kill "$RABBITMQ_MGMT_PID" 2>/dev/null || true
    fi

    kubectl port-forward svc/rabbitmq "$RABBITMQ_MGMT_PORT:15672" -n "$NS_SYSTEM" >/tmp/fluidbg-rabbitmq-port-forward.log 2>&1 &
    RABBITMQ_MGMT_PID=$!
    sleep 3
    if ! kill -0 "$RABBITMQ_MGMT_PID" 2>/dev/null; then
        echo "RabbitMQ management port-forward exited unexpectedly" >&2
        cat /tmp/fluidbg-rabbitmq-port-forward.log >&2 || true
        return 1
    fi
    wait_http "$RABBITMQ_MGMT_URL" rabbitmq-management
}

publish_rabbitmq_message() {
    local routing_key="$1"
    local payload="$2"
    local request response

    request="$(RABBITMQ_ROUTING_KEY="$routing_key" RABBITMQ_PAYLOAD="$payload" python3 - <<'PY'
import json
import os

print(json.dumps({
    "properties": {},
    "routing_key": os.environ["RABBITMQ_ROUTING_KEY"],
    "payload": os.environ["RABBITMQ_PAYLOAD"],
    "payload_encoding": "string",
}))
PY
)"

    for i in $(seq 1 10); do
        ensure_rabbitmq_management_port_forward
        response="$(curl -sf -u fluidbg:fluidbg \
            -H "Content-Type: application/json" \
            -X POST "$RABBITMQ_MGMT_URL/api/exchanges/%2F/amq.default/publish" \
            -d "$request" || true)"
        if [ -n "$response" ] && RABBITMQ_PUBLISH_RESPONSE="$response" python3 - <<'PY'
import json
import os
import sys

try:
    payload = json.loads(os.environ["RABBITMQ_PUBLISH_RESPONSE"])
except Exception:
    sys.exit(1)
sys.exit(0 if payload.get("routed") is True else 1)
PY
        then
            return 0
        fi
        echo "RabbitMQ publish to '$routing_key' was not routed, retrying... ($i/10)"
        sleep 1
    done

    echo "RabbitMQ publish to '$routing_key' was not routed after retries. Last response: ${response:-<empty>}" >&2
    return 1
}

queue_contains_processed_message() {
    local queue="$1"
    local recovery_token="$2"
    local instance_prefix="$3"
    local payload
    ensure_rabbitmq_management_port_forward
    payload="$(curl -sf -u fluidbg:fluidbg \
        -H "Content-Type: application/json" \
        -X POST "$RABBITMQ_MGMT_URL/api/queues/%2F/$queue/get" \
        -d '{"count":100,"ackmode":"ack_requeue_true","encoding":"auto","truncate":50000}' || true)"
    if [ -z "$payload" ]; then
        return 1
    fi
    PAYLOAD_JSON="$payload" python3 - "$recovery_token" "$instance_prefix" <<'PY'
import json
import os
import sys

token = sys.argv[1]
instance_prefix = sys.argv[2]
messages = json.loads(os.environ.get("PAYLOAD_JSON", "[]"))
for message in messages:
    payload = message.get("payload")
    if not isinstance(payload, str):
        continue
    try:
        decoded = json.loads(payload)
    except Exception:
        continue
    original = decoded.get("originalMessage") or {}
    instance_name = decoded.get("instanceName", "")
    if original.get("recoveryToken") == token and instance_name.startswith(instance_prefix + "-"):
        sys.exit(0)
sys.exit(1)
PY
}

queue_contains_json_field() {
    local queue="$1"
    local field="$2"
    local expected="$3"
    local payload
    ensure_rabbitmq_management_port_forward
    payload="$(curl -sf -u fluidbg:fluidbg \
        -H "Content-Type: application/json" \
        -X POST "$RABBITMQ_MGMT_URL/api/queues/%2F/$queue/get" \
        -d '{"count":100,"ackmode":"ack_requeue_true","encoding":"auto","truncate":50000}' || true)"
    if [ -z "$payload" ]; then
        return 1
    fi
    PAYLOAD_JSON="$payload" python3 - "$field" "$expected" <<'PY'
import json
import os
import sys

field = sys.argv[1]
expected = sys.argv[2]
messages = json.loads(os.environ.get("PAYLOAD_JSON", "[]"))
for message in messages:
    payload = message.get("payload")
    if not isinstance(payload, str):
        continue
    try:
        decoded = json.loads(payload)
    except Exception:
        continue
    if str(decoded.get(field)) == expected:
        sys.exit(0)
sys.exit(1)
PY
}

assert_rabbitmq_management_queue_drained() {
    local queue="$1"
    local response status payload
    ensure_rabbitmq_management_port_forward
    response="$(curl -s -u fluidbg:fluidbg -w $'\n%{http_code}' "$RABBITMQ_MGMT_URL/api/queues/%2F/$queue" || true)"
    status="${response##*$'\n'}"
    payload="${response%$'\n'*}"
    if [ "$status" = "404" ]; then
        echo "RabbitMQ queue '$queue' already deleted after drain"
        return 0
    fi
    if [ "$status" != "200" ] || [ -z "$payload" ]; then
        echo "RabbitMQ management API did not return queue '$queue' (status=$status)" >&2
        return 1
    fi
    PAYLOAD_JSON="$payload" python3 - "$queue" <<'PY'
import json
import os
import sys

queue = sys.argv[1]
payload = json.loads(os.environ["PAYLOAD_JSON"])
ready = int(payload.get("messages_ready", 0))
unacked = int(payload.get("messages_unacknowledged", 0))
consumers = int(payload.get("consumers", 0))
if ready == 0 and unacked == 0:
    sys.exit(0)
print(
    f"queue {queue} not drained via management API: ready={ready} unacked={unacked} consumers={consumers}",
    file=sys.stderr,
)
sys.exit(1)
PY
}

assert_bgd_has_no_drain_timeouts() {
    local bgd="$1"
    local namespace="$2"
    local status_json
    status_json="$(kubectl get bluegreendeployment "$bgd" -n "$namespace" -o json)"
    STATUS_JSON="$status_json" python3 - "$bgd" <<'PY'
import json
import os
import sys

bgd = sys.argv[1]
payload = json.loads(os.environ["STATUS_JSON"])
timeouts = [
    status
    for status in payload.get("status", {}).get("inceptionPointDrains", [])
    if status.get("phase") == "timedOutMaybeSuccessful"
]
if timeouts:
    print(f"{bgd} has drain timeout statuses: {timeouts}", file=sys.stderr)
    sys.exit(1)
PY
}

get_inception_config_value() {
    local bgd="$1"
    local inception_point="$2"
    local namespace="$3"
    local path="$4"
    local config
    config="$(kubectl get configmap -n "$namespace" -l "fluidbg.io/blue-green-ref=$bgd,fluidbg.io/inception-point=$inception_point" -o jsonpath='{.items[0].data.config\.yaml}' 2>/dev/null || true)"
    if [ -z "$config" ]; then
        echo "config map for bgd=$bgd inception=$inception_point not found" >&2
        return 1
    fi
    CONFIG_YAML="$config" python3 - "$path" <<'PY'
import os
import sys

target = sys.argv[1].split(".")
stack = []
for raw in os.environ["CONFIG_YAML"].splitlines():
    if not raw.strip() or raw.lstrip().startswith("#"):
        continue
    indent = len(raw) - len(raw.lstrip(" "))
    line = raw.strip()
    if ":" not in line or line.startswith("-"):
        continue
    key, value = line.split(":", 1)
    level = indent // 2
    stack = stack[:level] + [key.strip()]
    if stack == target:
        value = value.strip().strip('"').strip("'")
        print(value)
        sys.exit(0)
sys.exit(1)
PY
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

wait_no_inception_resources() {
    local namespace="$1"
    for i in $(seq 1 60); do
        local deployments services configmaps secrets pods
        deployments="$(kubectl get deployment -n "$namespace" -l fluidbg.io/inception-point --no-headers 2>/dev/null | wc -l | tr -d ' ')"
        services="$(kubectl get service -n "$namespace" -l fluidbg.io/inception-point --no-headers 2>/dev/null | wc -l | tr -d ' ')"
        configmaps="$(kubectl get configmap -n "$namespace" -l fluidbg.io/inception-point --no-headers 2>/dev/null | wc -l | tr -d ' ')"
        secrets="$(kubectl get secret -n "$namespace" -l fluidbg.io/inception-point --no-headers 2>/dev/null | wc -l | tr -d ' ')"
        pods="$(kubectl get pods -n "$namespace" -l fluidbg.io/inception-point --no-headers 2>/dev/null | wc -l | tr -d ' ')"
        if [ "$deployments" = "0" ] && [ "$services" = "0" ] && [ "$configmaps" = "0" ] && [ "$secrets" = "0" ] && [ "$pods" = "0" ]; then
            return 0
        fi
        echo "Waiting for old inception resources to disappear... deployments=$deployments services=$services configmaps=$configmaps secrets=$secrets pods=$pods ($i/60)"
        sleep 1
    done
    echo "old inception resources still exist in namespace $namespace" >&2
    kubectl get deployment,service,configmap,secret,pods -n "$namespace" -l fluidbg.io/inception-point >&2 || true
    return 1
}

wait_no_blue_green_ref_resources() {
    local namespace="$1"
    local bgd="$2"
    for i in $(seq 1 90); do
        local deployments services configmaps secrets pods
        deployments="$(kubectl get deployment -n "$namespace" -l "fluidbg.io/blue-green-ref=$bgd" --no-headers 2>/dev/null | wc -l | tr -d ' ')"
        services="$(kubectl get service -n "$namespace" -l "fluidbg.io/blue-green-ref=$bgd" --no-headers 2>/dev/null | wc -l | tr -d ' ')"
        configmaps="$(kubectl get configmap -n "$namespace" -l "fluidbg.io/blue-green-ref=$bgd" --no-headers 2>/dev/null | wc -l | tr -d ' ')"
        secrets="$(kubectl get secret -n "$namespace" -l "fluidbg.io/blue-green-ref=$bgd" --no-headers 2>/dev/null | wc -l | tr -d ' ')"
        pods="$(kubectl get pods -n "$namespace" -l "fluidbg.io/blue-green-ref=$bgd" --no-headers 2>/dev/null | wc -l | tr -d ' ')"
        if [ "$deployments" = "0" ] && [ "$services" = "0" ] && [ "$configmaps" = "0" ] && [ "$secrets" = "0" ] && [ "$pods" = "0" ]; then
            return 0
        fi
        echo "Waiting for orphaned resources for $bgd to disappear... deployments=$deployments services=$services configmaps=$configmaps secrets=$secrets pods=$pods ($i/90)"
        sleep 1
    done
    echo "orphaned resources for $bgd still exist in namespace $namespace" >&2
    kubectl get deployment,service,configmap,secret,pods -n "$namespace" -l "fluidbg.io/blue-green-ref=$bgd" >&2 || true
    return 1
}

cleanup_stale_blue_green_deployments() {
    if ! kubectl get crd bluegreendeployments.fluidbg.io >/dev/null 2>&1; then
        return 0
    fi

    kubectl get bluegreendeployments.fluidbg.io -A -o json 2>/dev/null | python3 -c 'import json,sys
data = json.load(sys.stdin)
for item in data.get("items", []):
    metadata = item.get("metadata", {})
    namespace = metadata.get("namespace")
    name = metadata.get("name")
    if namespace and name:
        print(namespace, name)
' | while read -r namespace name; do
        kubectl patch bluegreendeployment "$name" -n "$namespace" --type=merge -p '{"metadata":{"finalizers":[]}}' >/dev/null 2>&1 || true
        kubectl delete bluegreendeployment "$name" -n "$namespace" --ignore-not-found --wait=false >/dev/null 2>&1 || true
    done
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

wait_deployment_replicas() {
    local deployment="$1"
    local namespace="$2"
    local expected="$3"
    for i in $(seq 1 60); do
        local desired available
        desired="$(kubectl get deployment "$deployment" -n "$namespace" -o jsonpath='{.spec.replicas}' 2>/dev/null || true)"
        available="$(kubectl get deployment "$deployment" -n "$namespace" -o jsonpath='{.status.availableReplicas}' 2>/dev/null || true)"
        available="${available:-0}"
        if [ "$desired" = "$expected" ] && [ "$available" = "$expected" ]; then
            return 0
        fi
        echo "Waiting for deployment/$deployment replicas desired=$expected available=$expected, current desired=${desired:-<none>} available=$available ($i/60)"
        sleep 2
    done
    echo "deployment/$deployment did not reach replicas=$expected in namespace $namespace" >&2
    kubectl get deployment "$deployment" -n "$namespace" -o yaml >&2 || true
    return 1
}

get_deployment_env_value() {
    local deployment="$1"
    local namespace="$2"
    local env_name="$3"
    local deployment_json
    deployment_json="$(kubectl get deployment "$deployment" -n "$namespace" -o json 2>/dev/null || true)"
    DEPLOYMENT_JSON="$deployment_json" python3 - "$env_name" <<'PY' || true
import json
import os
import sys

env_name = sys.argv[1]
try:
    deployment = json.loads(os.environ.get("DEPLOYMENT_JSON", ""))
except Exception:
    sys.exit(1)

containers = deployment.get("spec", {}).get("template", {}).get("spec", {}).get("containers", [])
for container in containers:
    for env in container.get("env", []) or []:
        if env.get("name") == env_name:
            print(env.get("value", ""))
            sys.exit(0)
sys.exit(1)
PY
}

wait_deployment_env_value() {
    local deployment="$1"
    local namespace="$2"
    local env_name="$3"
    local expected="$4"
    for i in $(seq 1 60); do
        local value
        value="$(get_deployment_env_value "$deployment" "$namespace" "$env_name")"
        if [ "$value" = "$expected" ]; then
            return 0
        fi
        echo "Waiting for deployment/$deployment env $env_name=$expected, current=${value:-<none>} ($i/60)"
        sleep 1
    done
    echo "deployment/$deployment did not reach env $env_name=$expected in namespace $namespace" >&2
    kubectl get deployment "$deployment" -n "$namespace" -o yaml >&2 || true
    return 1
}

wait_deployment_env_pair_values() {
    local first_deployment="$1"
    local second_deployment="$2"
    local namespace="$3"
    local env_name="$4"
    local expected_a="$5"
    local expected_b="$6"
    local forbidden="$7"
    for i in $(seq 1 60); do
        local first_value second_value
        first_value="$(get_deployment_env_value "$first_deployment" "$namespace" "$env_name")"
        second_value="$(get_deployment_env_value "$second_deployment" "$namespace" "$env_name")"
        if [ "$first_value" != "$forbidden" ] && [ "$second_value" != "$forbidden" ] && [ -n "$first_value" ] && [ -n "$second_value" ] && [ "$first_value" != "$second_value" ]; then
            if { [ "$first_value" = "$expected_a" ] && [ "$second_value" = "$expected_b" ]; } || { [ "$first_value" = "$expected_b" ] && [ "$second_value" = "$expected_a" ]; }; then
                return 0
            fi
        fi
        echo "Waiting for deployments/$first_deployment,$second_deployment env $env_name to use distinct temp values $expected_a/$expected_b and not $forbidden, current=$first_value/$second_value ($i/60)"
        sleep 1
    done
    echo "deployments/$first_deployment,$second_deployment did not reach distinct temp env $env_name values in namespace $namespace" >&2
    kubectl get deployment "$first_deployment" -n "$namespace" -o yaml >&2 || true
    kubectl get deployment "$second_deployment" -n "$namespace" -o yaml >&2 || true
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

wait_bgd_phase() {
    local bgd="$1"
    local namespace="$2"
    local expected="$3"
    local attempts="${4:-60}"
    for i in $(seq 1 "$attempts"); do
        local phase
        phase="$(kubectl get bluegreendeployment "$bgd" -n "$namespace" -o jsonpath='{.status.phase}' 2>/dev/null || true)"
        if [ "$phase" = "$expected" ]; then
            return 0
        fi
        echo "Waiting for bluegreendeployment/$bgd phase $expected, current=${phase:-<none>} ($i/$attempts)"
        sleep 2
    done
    echo "bluegreendeployment/$bgd did not reach phase $expected in namespace $namespace" >&2
    kubectl get bluegreendeployment "$bgd" -n "$namespace" -o yaml >&2 || true
    return 1
}

assert_bgd_condition_status() {
    local bgd="$1"
    local namespace="$2"
    local condition_type="$3"
    local expected="$4"
    local actual
    local status_json
    status_json="$(kubectl get bluegreendeployment "$bgd" -n "$namespace" -o json)"
    actual="$(STATUS_JSON="$status_json" python3 - "$condition_type" <<'PY'
import json
import os
import sys

condition_type = sys.argv[1]
data = json.loads(os.environ["STATUS_JSON"])
for condition in data.get("status", {}).get("conditions", []):
    if condition.get("type") == condition_type:
        print(condition.get("status", ""))
        sys.exit(0)
print("")
PY
)"
    if [ "$actual" != "$expected" ]; then
        echo "Expected bluegreendeployment/$bgd condition $condition_type=$expected, got ${actual:-<none>}" >&2
        kubectl get bluegreendeployment "$bgd" -n "$namespace" -o yaml >&2 || true
        return 1
    fi
}

wait_inception_deployment_name() {
    local bgd="$1"
    local inception_point="$2"
    local namespace="$3"
    for i in $(seq 1 60); do
        local value
        value="$(kubectl get deployment -n "$namespace" -l "fluidbg.io/blue-green-ref=$bgd,fluidbg.io/inception-point=$inception_point" -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)"
        if [ -n "$value" ]; then
            printf '%s\n' "$value"
            return 0
        fi
        echo "Waiting for inception deployment bgd=$bgd point=$inception_point... ($i/60)" >&2
        sleep 1
    done
    echo "inception deployment not found for bgd=$bgd point=$inception_point in namespace $namespace" >&2
    kubectl get deployment -n "$namespace" -l "fluidbg.io/blue-green-ref=$bgd" >&2 || true
    return 1
}

wait_test_deployment_name() {
    local bgd="$1"
    local test_name="$2"
    local namespace="$3"
    for i in $(seq 1 60); do
        local value
        value="$(kubectl get deployment -n "$namespace" -l "fluidbg.io/blue-green-ref=$bgd,fluidbg.io/test=$test_name" -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)"
        if [ -n "$value" ]; then
            printf '%s\n' "$value"
            return 0
        fi
        echo "Waiting for test deployment bgd=$bgd test=$test_name... ($i/60)" >&2
        sleep 1
    done
    echo "test deployment not found for bgd=$bgd test=$test_name in namespace $namespace" >&2
    kubectl get deployment -n "$namespace" -l "fluidbg.io/blue-green-ref=$bgd,fluidbg.io/test=$test_name" >&2 || true
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
need_cmd helm
need_cmd curl
need_cmd python3
need_cmd cargo

echo ""
echo "--- Step 0: Regenerate CRDs ---"
cargo run -p fluidbg-operator --bin gen-crds
cp "$ROOT_DIR/crds/blue_green_deployment.yaml" "$ROOT_DIR/charts/fluidbg-operator/crds/blue_green_deployment.yaml"
cp "$ROOT_DIR/crds/inception_plugin.yaml" "$ROOT_DIR/charts/fluidbg-operator/crds/inception_plugin.yaml"

if [ "$BUILD_IMAGES" = "1" ]; then
    need_cmd docker

    echo ""
    echo "--- Step 0b: Prefetch Linux Rust dependencies ---"
    IMAGE_ARCH="$(target_arch)"
    prefetch_linux_rust_dependencies "$IMAGE_ARCH"
    echo ""
    echo "--- Step 0c: Build local images ---"
    build_linux_rust_binaries "$IMAGE_ARCH"
    docker build --platform "linux/$IMAGE_ARCH" -t fluidbg/fbg-operator:dev "$ROOT_DIR"
    docker build --platform "linux/$IMAGE_ARCH" -f "$ROOT_DIR/plugins/http/Dockerfile" -t fluidbg/fbg-plugin-http:dev "$ROOT_DIR"
    docker build --platform "linux/$IMAGE_ARCH" -f "$ROOT_DIR/plugins/rabbitmq/Dockerfile" -t fluidbg/fbg-plugin-rabbitmq:dev "$ROOT_DIR"
    docker build -t fluidbg/blue-app:dev "$ROOT_DIR/e2e/blue-app"
    docker build -t fluidbg/green-app:dev "$ROOT_DIR/e2e/green-app"
    docker build -t fluidbg/test-app:dev "$ROOT_DIR/e2e/test-app"
    if [ "$E2E_STATE_STORE" = "postgres" ]; then
        docker pull --platform "linux/$IMAGE_ARCH" postgres:18-alpine
    fi

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
        echo "--- Step 0d: Load images into kind cluster '$KIND_CLUSTER' ---"
        kind load docker-image fluidbg/fbg-operator:dev --name "$KIND_CLUSTER"
        kind load docker-image fluidbg/fbg-plugin-http:dev --name "$KIND_CLUSTER"
        kind load docker-image fluidbg/fbg-plugin-rabbitmq:dev --name "$KIND_CLUSTER"
        kind load docker-image fluidbg/blue-app:dev --name "$KIND_CLUSTER"
        kind load docker-image fluidbg/green-app:dev --name "$KIND_CLUSTER"
        kind load docker-image fluidbg/test-app:dev --name "$KIND_CLUSTER"
        if [ "$E2E_STATE_STORE" = "postgres" ]; then
            kind load docker-image postgres:18-alpine --name "$KIND_CLUSTER" || \
                echo "Warning: failed to preload postgres:18-alpine into kind; cluster image pull will be used"
        fi
    fi
fi

echo ""
echo "--- Step 1: Deploy infrastructure ---"
apply_namespace "$NS_SYSTEM"
apply_namespace "$NS"
kubectl apply -f "$DEPLOY_DIR/01-httpbin.yaml"
kubectl apply -f "$DEPLOY_DIR/01-rabbitmq.yaml"
if [ "$E2E_STATE_STORE" = "postgres" ]; then
    kubectl apply -f "$DEPLOY_DIR/01-postgres.yaml"
fi
reset_deployment "$NS_SYSTEM" rabbitmq rabbitmq
kubectl rollout status deployment/rabbitmq -n "$NS_SYSTEM" --timeout=120s
kubectl rollout status deployment/httpbin -n "$NS_SYSTEM" --timeout=120s
if [ "$E2E_STATE_STORE" = "postgres" ]; then
    kubectl rollout status deployment/postgres -n "$NS_SYSTEM" --timeout=120s
fi
echo "Waiting for RabbitMQ readiness (extra 15s)..."
sleep 15

echo ""
echo "--- Step 2: Reset previous chart release and test resources ---"
cleanup_stale_blue_green_deployments
helm uninstall fluidbg-e2e -n "$NS_SYSTEM" --ignore-not-found --wait >/dev/null 2>&1 || true
kubectl delete deployment -n "$NS" -l fluidbg.io/name=order-processor --ignore-not-found
kubectl delete deployment test-container -n "$NS" --ignore-not-found
kubectl delete service test-container -n "$NS" --ignore-not-found
kubectl delete deployment,service -n "$NS" -l fluidbg.io/test --ignore-not-found
kubectl delete deployment,service,configmap -n "$NS" -l fluidbg.io/inception-point --ignore-not-found
kubectl delete pod -n "$NS" -l fluidbg.io/inception-point --ignore-not-found
if [ "$E2E_STATE_STORE" != "postgres" ]; then
    kubectl delete deployment postgres -n "$NS_SYSTEM" --ignore-not-found
    kubectl delete service postgres -n "$NS_SYSTEM" --ignore-not-found
    kubectl delete secret fluidbg-postgres -n "$NS_SYSTEM" --ignore-not-found
fi
kubectl delete crd \
    bluegreendeployments.fluidbg.io \
    inceptionplugins.fluidbg.io \
    --ignore-not-found \
    --wait=true
wait_deleted deployment test-container "$NS"
wait_deleted service test-container "$NS"
wait_no_inception_resources "$NS"

echo ""
echo "--- Step 3: Deploy operator and built-in plugins with Helm ---"
if [ "$E2E_STATE_STORE" != "memory" ] && [ "$E2E_STATE_STORE" != "postgres" ]; then
    echo "Unsupported E2E_STATE_STORE=$E2E_STATE_STORE" >&2
    exit 1
fi

install_operator_chart() {
    local helm_args=(
        upgrade --install fluidbg-e2e "$ROOT_DIR/charts/fluidbg-operator"
        --namespace "$NS_SYSTEM" \
        --create-namespace \
        --set fullnameOverride=fluidbg-operator \
        --set serviceAccount.name=fluidbg-operator \
        --set operator.replicaCount="$OPERATOR_REPLICAS" \
        --set operator.image.repository=fluidbg/fbg-operator \
        --set operator.image.tag=dev \
        --set operator.image.pullPolicy=Never \
        --set operator.watchNamespace="$NS" \
        --set operator.orphanCleanup.intervalSeconds=5 \
        --set operator.auth.createSigningSecret=true \
        --set operator.auth.signingSecretNamespace="$NS_SYSTEM" \
        --set operator.auth.signingSecretName=fluidbg-e2e-auth \
        --set operator.auth.signingSecretKey=signing-key \
        --set-string operator.auth.signingSecretValue=fluidbg-e2e-signing-key \
        --set builtinPlugins.namespaces[0]="$NS" \
        --set builtinPlugins.http.image.repository=fluidbg/fbg-plugin-http \
        --set builtinPlugins.http.image.tag=dev \
        --set builtinPlugins.rabbitmq.image.repository=fluidbg/fbg-plugin-rabbitmq \
        --set builtinPlugins.rabbitmq.image.tag=dev \
        --set builtinPlugins.rabbitmq.manager.enabled=true \
        --set builtinPlugins.rabbitmq.manager.amqpUrl="amqp://fluidbg:fluidbg@rabbitmq.fluidbg-system:5672/%2f" \
        --set builtinPlugins.azureServiceBus.enabled=false
    )
    if [ "$E2E_STATE_STORE" = "postgres" ]; then
        helm_args+=(
            --set stateStore.type=postgres
            --set stateStore.postgres.authMode=password
            --set stateStore.postgres.urlSecretName=fluidbg-postgres
            --set stateStore.postgres.urlSecretKey=url
            --set stateStore.postgres.tableName=fluidbg_cases
        )
    fi
    helm "${helm_args[@]}"
}

install_operator_chart
kubectl rollout status deployment/fluidbg-operator -n "$NS_SYSTEM" --timeout=120s
kubectl rollout status deployment/fluidbg-rabbitmq-manager -n "$NS_SYSTEM" --timeout=120s
wait_exists inceptionplugin http "$NS"
wait_exists inceptionplugin rabbitmq "$NS"

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
UPGRADE_INPUT_PLUGIN_DEPLOYMENT="$(wait_inception_deployment_name order-processor-upgrade incoming-orders "$NS")"
UPGRADE_OUTPUT_PLUGIN_DEPLOYMENT="$(wait_inception_deployment_name order-processor-upgrade outgoing-results "$NS")"
UPGRADE_TEST_DEPLOYMENT="$(wait_test_deployment_name order-processor-upgrade test-container "$NS")"
wait_exists deployment "$UPGRADE_DEPLOYMENT" "$NS"
kubectl rollout status deployment/"$BOOTSTRAP_DEPLOYMENT" -n "$NS" --timeout=120s
kubectl rollout status deployment/"$UPGRADE_DEPLOYMENT" -n "$NS" --timeout=120s
kubectl rollout status deployment/"$UPGRADE_TEST_DEPLOYMENT" -n "$NS" --timeout=120s
kubectl rollout status deployment/"$UPGRADE_INPUT_PLUGIN_DEPLOYMENT" -n "$NS" --timeout=120s
kubectl rollout status deployment/"$UPGRADE_OUTPUT_PLUGIN_DEPLOYMENT" -n "$NS" --timeout=120s
wait_bgd_phase order-processor-upgrade "$NS" Observing 60

echo ""
echo "--- Step 6: Check BGD status ---"
kubectl get bluegreendeployment order-processor-bootstrap -n "$NS" -o yaml
kubectl get bluegreendeployment order-processor-upgrade -n "$NS" -o yaml

echo ""
echo "--- Step 7: Port-forward RabbitMQ management API ---"
ensure_rabbitmq_management_port_forward

echo ""
echo "--- Step 8: Publish input messages to RabbitMQ ---"
for i in 1 2 3 4 5; do
    echo "Publishing order-$i..."
    publish_rabbitmq_message "orders" "{\"orderId\":\"order-$i\",\"type\":\"order\",\"action\":\"process\"}"
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
wait_deleted deployment "$UPGRADE_TEST_DEPLOYMENT" "$NS"
wait_deleted service "$UPGRADE_TEST_DEPLOYMENT" "$NS"
wait_no_inception_resources "$NS"
PROMOTED_GREEN="$(kubectl get deployment "$UPGRADE_DEPLOYMENT" -n "$NS" -o jsonpath='{.metadata.labels.fluidbg\.io/green}')"
if [ "$PROMOTED_GREEN" != "true" ]; then
    echo "Expected promoted deployment to have fluidbg.io/green=true, got '$PROMOTED_GREEN'" >&2
    kubectl get deployment "$UPGRADE_DEPLOYMENT" -n "$NS" -o yaml
    exit 1
fi

echo ""
echo "--- Step 11: Final successful promotion status ---"
kubectl get bluegreendeployment order-processor-upgrade -n "$NS" -o jsonpath='{.status}' | python3 -m json.tool
assert_bgd_condition_status order-processor-upgrade "$NS" Ready True
assert_bgd_condition_status order-processor-upgrade "$NS" Progressing False
assert_bgd_condition_status order-processor-upgrade "$NS" Degraded False

echo ""
echo "--- Step 12: Check pods after successful promotion ---"
kubectl get pods -n "$NS"
kubectl get pods -n "$NS_SYSTEM"

echo ""
echo "--- Step 13: Deploy failing upgrade BGD ---"
kubectl apply -f "$DEPLOY_DIR/07-failing-upgrade-bgd.yaml"
sleep 5
FAILING_DEPLOYMENT="$(wait_bgd_generated_name order-processor-failing-upgrade "$NS")"
FAILING_INPUT_PLUGIN_DEPLOYMENT="$(wait_inception_deployment_name order-processor-failing-upgrade incoming-orders "$NS")"
FAILING_OUTPUT_PLUGIN_DEPLOYMENT="$(wait_inception_deployment_name order-processor-failing-upgrade outgoing-results "$NS")"
FAILING_TEST_DEPLOYMENT="$(wait_test_deployment_name order-processor-failing-upgrade test-container "$NS")"
wait_exists deployment "$FAILING_DEPLOYMENT" "$NS"
kubectl rollout status deployment/"$UPGRADE_DEPLOYMENT" -n "$NS" --timeout=120s
kubectl rollout status deployment/"$FAILING_DEPLOYMENT" -n "$NS" --timeout=120s
kubectl rollout status deployment/"$FAILING_TEST_DEPLOYMENT" -n "$NS" --timeout=120s
kubectl rollout status deployment/"$FAILING_INPUT_PLUGIN_DEPLOYMENT" -n "$NS" --timeout=120s
kubectl rollout status deployment/"$FAILING_OUTPUT_PLUGIN_DEPLOYMENT" -n "$NS" --timeout=120s
wait_bgd_phase order-processor-failing-upgrade "$NS" Observing 60

FAILING_GREEN_INPUT_QUEUE="$(get_inception_config_value order-processor-failing-upgrade incoming-orders "$NS" duplicator.greenInputQueue)"
FAILING_GREEN_OUTPUT_QUEUE="$(get_inception_config_value order-processor-failing-upgrade outgoing-results "$NS" combiner.greenOutputQueue)"
SHADOW_SUFFIX="_dlq"
FAILING_GREEN_INPUT_SHADOW_QUEUE="${FAILING_GREEN_INPUT_QUEUE}${SHADOW_SUFFIX}"
FAILING_GREEN_OUTPUT_SHADOW_QUEUE="${FAILING_GREEN_OUTPUT_QUEUE}${SHADOW_SUFFIX}"
BASE_INPUT_SHADOW_QUEUE="orders${SHADOW_SUFFIX}"
BASE_OUTPUT_SHADOW_QUEUE="results${SHADOW_SUFFIX}"
SHADOW_INPUT_TOKEN="input-shadow-rollback-$(date +%s)"
SHADOW_OUTPUT_TOKEN="output-shadow-rollback-$(date +%s)"

echo "Publishing shadow input recovery message to $FAILING_GREEN_INPUT_SHADOW_QUEUE..."
publish_rabbitmq_message "$FAILING_GREEN_INPUT_SHADOW_QUEUE" "{\"shadowToken\":\"$SHADOW_INPUT_TOKEN\",\"type\":\"order\"}"
echo "Publishing shadow output recovery message to $FAILING_GREEN_OUTPUT_SHADOW_QUEUE..."
publish_rabbitmq_message "$FAILING_GREEN_OUTPUT_SHADOW_QUEUE" "{\"shadowToken\":\"$SHADOW_OUTPUT_TOKEN\",\"type\":\"result\"}"

echo ""
echo "--- Step 14: Publish failing input messages ---"
RECOVERY_TOKEN="rollback-recovery-1"
echo "Publishing delayed recovery message $RECOVERY_TOKEN..."
publish_rabbitmq_message "orders" "{\"type\":\"order\",\"action\":\"process\",\"recoveryToken\":\"$RECOVERY_TOKEN\",\"greenInitialProcessingDelaySeconds\":30}"
sleep 2
for i in 1 2 3 4 5; do
    echo "Publishing failing order-$i..."
    publish_rabbitmq_message "orders" "{\"orderId\":\"fail-$i\",\"type\":\"order\",\"action\":\"process\",\"shouldPass\":false,\"failureReason\":\"synthetic failed promotion case\"}"
    sleep 1
done

echo ""
echo "--- Step 15: Wait for rollback ---"
for i in $(seq 1 40); do
    STATUS_JSON="$(kubectl get bluegreendeployment order-processor-failing-upgrade -n "$NS" -o json)"
    PHASE="$(printf '%s' "$STATUS_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status", {}).get("phase", ""))')"
    FAILED_COUNT="$(printf '%s' "$STATUS_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status", {}).get("testCasesFailed", 0))')"
    echo "  Failing BGD phase: ${PHASE:-<none>} failed=$FAILED_COUNT ($i/40)"
    if [ "$PHASE" = "RolledBack" ]; then
        break
    fi
    sleep 5
done

PHASE="$(kubectl get bluegreendeployment order-processor-failing-upgrade -n "$NS" -o jsonpath='{.status.phase}')"
if [ "$PHASE" != "RolledBack" ]; then
    echo "Expected failing BGD phase RolledBack, got '$PHASE'" >&2
    kubectl get bluegreendeployment order-processor-failing-upgrade -n "$NS" -o yaml >&2
    exit 1
fi

assert_bgd_has_no_drain_timeouts order-processor-failing-upgrade "$NS"
wait_deleted deployment "$FAILING_DEPLOYMENT" "$NS"
wait_deployment_label "$UPGRADE_DEPLOYMENT" "$NS" '{.metadata.labels.fluidbg\.io/green}' true
wait_deleted deployment "$FAILING_TEST_DEPLOYMENT" "$NS"
wait_deleted service "$FAILING_TEST_DEPLOYMENT" "$NS"
assert_rabbitmq_management_queue_drained "orders-green"
assert_rabbitmq_management_queue_drained "orders-blue"
assert_rabbitmq_management_queue_drained "results-green"
assert_rabbitmq_management_queue_drained "results-blue"
assert_rabbitmq_management_queue_drained "$FAILING_GREEN_INPUT_SHADOW_QUEUE"
assert_rabbitmq_management_queue_drained "$FAILING_GREEN_OUTPUT_SHADOW_QUEUE"
wait_no_inception_resources "$NS"

echo ""
echo "--- Step 16: Verify stranded green message recovery ---"
for i in $(seq 1 30); do
    if queue_contains_processed_message "results" "$RECOVERY_TOKEN" "$UPGRADE_DEPLOYMENT"; then
        echo "Recovered message $RECOVERY_TOKEN was processed by restored green deployment $UPGRADE_DEPLOYMENT"
        break
    fi
    echo "  Waiting for recovered message $RECOVERY_TOKEN on results... ($i/30)"
    sleep 2
done

if ! queue_contains_processed_message "results" "$RECOVERY_TOKEN" "$UPGRADE_DEPLOYMENT"; then
    echo "Expected recovered message $RECOVERY_TOKEN to be processed by restored green deployment $UPGRADE_DEPLOYMENT" >&2
    ensure_rabbitmq_management_port_forward
    curl -sf -u fluidbg:fluidbg \
        -H "Content-Type: application/json" \
        -X POST "$RABBITMQ_MGMT_URL/api/queues/%2F/results/get" \
        -d '{"count":20,"ackmode":"ack_requeue_true","encoding":"auto","truncate":50000}' | python3 -m json.tool >&2 || true
    exit 1
fi

echo ""
echo "--- Step 16b: Verify shadow queue recovery ---"
for i in $(seq 1 30); do
    if queue_contains_json_field "$BASE_INPUT_SHADOW_QUEUE" shadowToken "$SHADOW_INPUT_TOKEN" \
        && queue_contains_json_field "$BASE_OUTPUT_SHADOW_QUEUE" shadowToken "$SHADOW_OUTPUT_TOKEN"; then
        echo "Shadow queue messages were moved back to base shadow queues"
        break
    fi
    echo "  Waiting for shadow queue recovery... ($i/30)"
    sleep 2
done

if ! queue_contains_json_field "$BASE_INPUT_SHADOW_QUEUE" shadowToken "$SHADOW_INPUT_TOKEN"; then
    echo "Expected input shadow message to be moved to $BASE_INPUT_SHADOW_QUEUE" >&2
    exit 1
fi
if ! queue_contains_json_field "$BASE_OUTPUT_SHADOW_QUEUE" shadowToken "$SHADOW_OUTPUT_TOKEN"; then
    echo "Expected output shadow message to be moved to $BASE_OUTPUT_SHADOW_QUEUE" >&2
    exit 1
fi

echo ""
echo "--- Step 17: Final failed promotion status ---"
kubectl get bluegreendeployment order-processor-failing-upgrade -n "$NS" -o jsonpath='{.status}' | python3 -m json.tool
assert_bgd_condition_status order-processor-failing-upgrade "$NS" Ready False
assert_bgd_condition_status order-processor-failing-upgrade "$NS" Progressing False
assert_bgd_condition_status order-processor-failing-upgrade "$NS" Degraded True

echo ""
echo "--- Step 18: Verify unsupported progressive plugin is rejected ---"
wait_no_inception_resources "$NS"
kubectl apply -f "$DEPLOY_DIR/08-progressive-unsupported.yaml"
UNSUPPORTED_DEPLOYMENT="$(wait_bgd_generated_name order-processor-progressive-unsupported "$NS")"
sleep 8
if kubectl get deployment "$UNSUPPORTED_DEPLOYMENT" -n "$NS" >/dev/null 2>&1; then
    echo "Unsupported progressive plugin created candidate deployment $UNSUPPORTED_DEPLOYMENT" >&2
    kubectl get bluegreendeployment order-processor-progressive-unsupported -n "$NS" -o yaml >&2
    exit 1
fi
UNSUPPORTED_PHASE="$(kubectl get bluegreendeployment order-processor-progressive-unsupported -n "$NS" -o jsonpath='{.status.phase}' 2>/dev/null || true)"
if [ "$UNSUPPORTED_PHASE" = "Observing" ] || [ "$UNSUPPORTED_PHASE" = "Completed" ]; then
    echo "Unsupported progressive plugin reached unexpected phase $UNSUPPORTED_PHASE" >&2
    kubectl get bluegreendeployment order-processor-progressive-unsupported -n "$NS" -o yaml >&2
    exit 1
fi
kubectl delete bluegreendeployment order-processor-progressive-unsupported -n "$NS" --ignore-not-found
kubectl delete inceptionplugin rabbitmq-no-progressive -n "$NS" --ignore-not-found
wait_deleted bluegreendeployment order-processor-progressive-unsupported "$NS"
wait_deleted inceptionplugin rabbitmq-no-progressive "$NS"
wait_no_inception_resources "$NS"

echo ""
echo "--- Step 19: Deploy progressive splitter BGD ---"
wait_no_inception_resources "$NS"
kubectl apply -f "$DEPLOY_DIR/09-progressive-upgrade-bgd.yaml"
sleep 5
PROGRESSIVE_DEPLOYMENT="$(wait_bgd_generated_name order-processor-progressive-upgrade "$NS")"
PROGRESSIVE_INPUT_PLUGIN_DEPLOYMENT="$(wait_inception_deployment_name order-processor-progressive-upgrade incoming-orders "$NS")"
PROGRESSIVE_OUTPUT_PLUGIN_DEPLOYMENT="$(wait_inception_deployment_name order-processor-progressive-upgrade outgoing-results "$NS")"
PROGRESSIVE_TEST_DEPLOYMENT="$(wait_test_deployment_name order-processor-progressive-upgrade test-container "$NS")"
wait_exists deployment "$PROGRESSIVE_DEPLOYMENT" "$NS"
kubectl rollout status deployment/"$UPGRADE_DEPLOYMENT" -n "$NS" --timeout=120s
kubectl rollout status deployment/"$PROGRESSIVE_DEPLOYMENT" -n "$NS" --timeout=120s
kubectl rollout status deployment/"$PROGRESSIVE_TEST_DEPLOYMENT" -n "$NS" --timeout=120s
kubectl rollout status deployment/"$PROGRESSIVE_INPUT_PLUGIN_DEPLOYMENT" -n "$NS" --timeout=120s
kubectl rollout status deployment/"$PROGRESSIVE_OUTPUT_PLUGIN_DEPLOYMENT" -n "$NS" --timeout=120s
wait_bgd_phase order-processor-progressive-upgrade "$NS" Observing 60
sleep 5
PROGRESSIVE_INPUT_POD_BEFORE="$(kubectl get pod -n "$NS" -l "app=$PROGRESSIVE_INPUT_PLUGIN_DEPLOYMENT" -o jsonpath='{.items[0].metadata.name}')"

echo ""
echo "--- Step 20: Publish progressive splitter messages ---"
PROGRESSIVE_TARGET_PUBLISHED=120
PROGRESSIVE_PUBLISHED=0
publish_progressive_message() {
    PROGRESSIVE_PUBLISHED=$((PROGRESSIVE_PUBLISHED + 1))
    publish_rabbitmq_message "orders" "{\"orderId\":\"progressive-$PROGRESSIVE_PUBLISHED\",\"type\":\"progressive-order\",\"action\":\"process\"}"
}

echo ""
echo "--- Step 21: Wait for progressive promotion and green-route skip semantics ---"
PROGRESSIVE_LIVE_SHIFT_VERIFIED=false
for i in $(seq 1 180); do
    STATUS_JSON="$(kubectl get bluegreendeployment order-processor-progressive-upgrade -n "$NS" -o json)"
    OBSERVED_COUNT="$(printf '%s' "$STATUS_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status", {}).get("testCasesObserved", 0))')"
    PENDING_COUNT="$(printf '%s' "$STATUS_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status", {}).get("testCasesPending", 0))')"
    TRAFFIC_PERCENT="$(printf '%s' "$STATUS_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status", {}).get("currentTrafficPercent", 0))')"
    PHASE="$(printf '%s' "$STATUS_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status", {}).get("phase", ""))')"
    echo "  Progressive phase=${PHASE:-<none>} traffic=$TRAFFIC_PERCENT observed=$OBSERVED_COUNT pending=$PENDING_COUNT published=$PROGRESSIVE_PUBLISHED ($i/180)"
    if [ "$TRAFFIC_PERCENT" = "100" ] && [ "$PROGRESSIVE_LIVE_SHIFT_VERIFIED" != "true" ] && [ "$PHASE" = "Observing" ]; then
        PROGRESSIVE_INPUT_POD_CURRENT="$(kubectl get pod -n "$NS" -l "app=$PROGRESSIVE_INPUT_PLUGIN_DEPLOYMENT" -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)"
        if [ "$PROGRESSIVE_INPUT_POD_CURRENT" != "$PROGRESSIVE_INPUT_POD_BEFORE" ]; then
            echo "Expected progressive traffic shift to update live without restarting splitter pod; before=$PROGRESSIVE_INPUT_POD_BEFORE current=$PROGRESSIVE_INPUT_POD_CURRENT" >&2
            kubectl get pods -n "$NS" -l "app=$PROGRESSIVE_INPUT_PLUGIN_DEPLOYMENT" -o wide >&2 || true
            exit 1
        fi
        PROGRESSIVE_LIVE_SHIFT_VERIFIED=true
        while [ "$PROGRESSIVE_PUBLISHED" -lt "$PROGRESSIVE_TARGET_PUBLISHED" ]; do
            publish_progressive_message
        done
    fi
    if [ "$PHASE" = "Completed" ]; then
        break
    fi
    if [ "$PHASE" = "RolledBack" ]; then
        echo "Progressive BGD rolled back" >&2
        kubectl get bluegreendeployment order-processor-progressive-upgrade -n "$NS" -o yaml >&2
        exit 1
    fi
    if [ "$PROGRESSIVE_LIVE_SHIFT_VERIFIED" != "true" ] && [ "$OBSERVED_COUNT" -lt 1 ]; then
        publish_progressive_message
    fi
    sleep 1
done

PROGRESSIVE_STATUS="$(kubectl get bluegreendeployment order-processor-progressive-upgrade -n "$NS" -o json)"
PROGRESSIVE_PHASE="$(printf '%s' "$PROGRESSIVE_STATUS" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status", {}).get("phase", ""))')"
PROGRESSIVE_OBSERVED="$(printf '%s' "$PROGRESSIVE_STATUS" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status", {}).get("testCasesObserved", 0))')"
PROGRESSIVE_PENDING="$(printf '%s' "$PROGRESSIVE_STATUS" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status", {}).get("testCasesPending", 0))')"
if [ "$PROGRESSIVE_PHASE" != "Completed" ]; then
    echo "Expected progressive BGD phase Completed, got '$PROGRESSIVE_PHASE'" >&2
    kubectl get bluegreendeployment order-processor-progressive-upgrade -n "$NS" -o yaml >&2
    exit 1
fi
if [ "$PROGRESSIVE_OBSERVED" -ge "$PROGRESSIVE_TARGET_PUBLISHED" ]; then
    echo "Expected green-routed progressive messages to be skipped by operator registration; observed=$PROGRESSIVE_OBSERVED published=$PROGRESSIVE_TARGET_PUBLISHED" >&2
    kubectl get bluegreendeployment order-processor-progressive-upgrade -n "$NS" -o yaml >&2
    exit 1
fi
if [ "$PROGRESSIVE_PENDING" -ne 0 ]; then
    echo "Expected no pending progressive cases after completion, got $PROGRESSIVE_PENDING" >&2
    kubectl get bluegreendeployment order-processor-progressive-upgrade -n "$NS" -o yaml >&2
    exit 1
fi
if [ "$PROGRESSIVE_LIVE_SHIFT_VERIFIED" != "true" ]; then
    echo "Expected to observe a live progressive traffic shift before cleanup" >&2
    kubectl get bluegreendeployment order-processor-progressive-upgrade -n "$NS" -o yaml >&2
    exit 1
fi
wait_deleted deployment "$UPGRADE_DEPLOYMENT" "$NS"
wait_deleted deployment "$PROGRESSIVE_TEST_DEPLOYMENT" "$NS"
wait_deleted service "$PROGRESSIVE_TEST_DEPLOYMENT" "$NS"
wait_no_inception_resources "$NS"
wait_deployment_label "$PROGRESSIVE_DEPLOYMENT" "$NS" '{.metadata.labels.fluidbg\.io/green}' true

echo ""
echo "--- Step 22: Verify combined HTTP plugin observer/proxy path ---"
wait_no_inception_resources "$NS"
kubectl get inceptionplugin http -n "$NS" >/dev/null
kubectl apply -f "$DEPLOY_DIR/10-http-plugin-bgd.yaml"
sleep 5
HTTP_DEPLOYMENT="$(wait_bgd_generated_name order-processor-http-upgrade "$NS")"
HTTP_INPUT_PLUGIN_DEPLOYMENT="$(wait_inception_deployment_name order-processor-http-upgrade incoming-orders "$NS")"
HTTP_PROXY_PLUGIN_DEPLOYMENT="$(wait_inception_deployment_name order-processor-http-upgrade http-upstream "$NS")"
HTTP_OUTPUT_PLUGIN_DEPLOYMENT="$(wait_inception_deployment_name order-processor-http-upgrade outgoing-results "$NS")"
HTTP_TEST_DEPLOYMENT="$(wait_test_deployment_name order-processor-http-upgrade test-container "$NS")"
wait_exists deployment "$HTTP_DEPLOYMENT" "$NS"
kubectl rollout status deployment/"$PROGRESSIVE_DEPLOYMENT" -n "$NS" --timeout=120s
kubectl rollout status deployment/"$HTTP_DEPLOYMENT" -n "$NS" --timeout=120s
kubectl rollout status deployment/"$HTTP_TEST_DEPLOYMENT" -n "$NS" --timeout=120s
kubectl rollout status deployment/"$HTTP_INPUT_PLUGIN_DEPLOYMENT" -n "$NS" --timeout=120s
kubectl rollout status deployment/"$HTTP_PROXY_PLUGIN_DEPLOYMENT" -n "$NS" --timeout=120s
kubectl rollout status deployment/"$HTTP_OUTPUT_PLUGIN_DEPLOYMENT" -n "$NS" --timeout=120s
wait_bgd_phase order-processor-http-upgrade "$NS" Observing 60
wait_deployment_replicas "$HTTP_DEPLOYMENT" "$NS" 1

echo "Publishing HTTP proxy verification message..."
publish_rabbitmq_message "orders" '{"orderId":"http-proxy-1","type":"order","action":"http-proxy-check"}'

HTTP_CASE_VERIFIED=false
for i in $(seq 1 40); do
    STATUS_JSON="$(kubectl get bluegreendeployment order-processor-http-upgrade -n "$NS" -o json)"
    PHASE="$(printf '%s' "$STATUS_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status", {}).get("phase", ""))')"
    PASSED_COUNT="$(printf '%s' "$STATUS_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status", {}).get("testCasesPassed", 0))')"
    PENDING_COUNT="$(printf '%s' "$STATUS_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status", {}).get("testCasesPending", 0))')"
    echo "  HTTP BGD phase=${PHASE:-<none>} passed=$PASSED_COUNT pending=$PENDING_COUNT ($i/40)"
    if [ "$PASSED_COUNT" -ge 1 ] && [ "$HTTP_CASE_VERIFIED" != "true" ]; then
        HTTP_CASE_FLAGS="$(kubectl exec -n "$NS" deploy/"$HTTP_TEST_DEPLOYMENT" -- python -c 'import json,urllib.request; case=json.load(urllib.request.urlopen("http://localhost:8080/cases")).get("http-proxy-1", {}); print(json.dumps({"status": case.get("status"), "output_message_seen": bool(case.get("output_message_seen")), "http_call_seen": bool(case.get("http_call_seen"))}))')"
        HTTP_OUTPUT_SEEN="$(printf '%s' "$HTTP_CASE_FLAGS" | python3 -c 'import json,sys; print(str(json.load(sys.stdin).get("output_message_seen")).lower())')"
        HTTP_CALL_SEEN="$(printf '%s' "$HTTP_CASE_FLAGS" | python3 -c 'import json,sys; print(str(json.load(sys.stdin).get("http_call_seen")).lower())')"
        HTTP_CASE_STATUS="$(printf '%s' "$HTTP_CASE_FLAGS" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status", ""))')"
        if [ "$HTTP_CASE_STATUS" != "passed" ] || [ "$HTTP_OUTPUT_SEEN" != "true" ] || [ "$HTTP_CALL_SEEN" != "true" ]; then
            echo "Expected HTTP proxy case to pass only after output message and HTTP call were observed, got $HTTP_CASE_FLAGS" >&2
            kubectl get bluegreendeployment order-processor-http-upgrade -n "$NS" -o yaml >&2
            exit 1
        fi
        HTTP_CASE_VERIFIED=true
    fi
    if [ "$PHASE" = "Completed" ] && [ "$PASSED_COUNT" -ge 1 ]; then
        break
    fi
    if [ "$PHASE" = "RolledBack" ]; then
        echo "HTTP plugin BGD rolled back" >&2
        kubectl get bluegreendeployment order-processor-http-upgrade -n "$NS" -o yaml >&2
        exit 1
    fi
    sleep 5
done

HTTP_STATUS="$(kubectl get bluegreendeployment order-processor-http-upgrade -n "$NS" -o json)"
HTTP_PHASE="$(printf '%s' "$HTTP_STATUS" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status", {}).get("phase", ""))')"
HTTP_PASSED="$(printf '%s' "$HTTP_STATUS" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status", {}).get("testCasesPassed", 0))')"
if [ "$HTTP_PHASE" != "Completed" ] || [ "$HTTP_PASSED" -lt 1 ]; then
    echo "Expected HTTP plugin BGD Completed with at least one passed test, got phase=$HTTP_PHASE passed=$HTTP_PASSED" >&2
    kubectl get bluegreendeployment order-processor-http-upgrade -n "$NS" -o yaml >&2
    exit 1
fi
if [ "$HTTP_CASE_VERIFIED" != "true" ]; then
    echo "Expected to verify HTTP proxy case before test deployment cleanup" >&2
    kubectl get bluegreendeployment order-processor-http-upgrade -n "$NS" -o yaml >&2
    exit 1
fi
wait_deleted deployment "$PROGRESSIVE_DEPLOYMENT" "$NS"
wait_deleted deployment "$HTTP_TEST_DEPLOYMENT" "$NS"
wait_deleted service "$HTTP_TEST_DEPLOYMENT" "$NS"
wait_no_inception_resources "$NS"
wait_deployment_label "$HTTP_DEPLOYMENT" "$NS" '{.metadata.labels.fluidbg\.io/green}' true
wait_deployment_replicas "$HTTP_DEPLOYMENT" "$NS" 2

echo ""
echo "--- Step 23: Force-delete BGD and verify orphan cleanup ---"
kubectl apply -f "$DEPLOY_DIR/11-force-delete-bgd.yaml"
sleep 5
FORCE_DELETE_DEPLOYMENT="$(wait_bgd_generated_name order-processor-force-delete "$NS")"
FORCE_DELETE_INPUT_PLUGIN_DEPLOYMENT="$(wait_inception_deployment_name order-processor-force-delete incoming-orders "$NS")"
FORCE_DELETE_OUTPUT_PLUGIN_DEPLOYMENT="$(wait_inception_deployment_name order-processor-force-delete outgoing-results "$NS")"
FORCE_DELETE_TEST_DEPLOYMENT="$(wait_test_deployment_name order-processor-force-delete test-container "$NS")"
wait_exists deployment "$FORCE_DELETE_DEPLOYMENT" "$NS"
kubectl rollout status deployment/"$HTTP_DEPLOYMENT" -n "$NS" --timeout=120s
kubectl rollout status deployment/"$FORCE_DELETE_DEPLOYMENT" -n "$NS" --timeout=120s
kubectl rollout status deployment/"$FORCE_DELETE_TEST_DEPLOYMENT" -n "$NS" --timeout=120s
kubectl rollout status deployment/"$FORCE_DELETE_INPUT_PLUGIN_DEPLOYMENT" -n "$NS" --timeout=120s
kubectl rollout status deployment/"$FORCE_DELETE_OUTPUT_PLUGIN_DEPLOYMENT" -n "$NS" --timeout=120s
wait_bgd_phase order-processor-force-delete "$NS" Observing 60
FORCE_DELETE_BLUE_INPUT_QUEUE="$(get_inception_config_value order-processor-force-delete incoming-orders "$NS" duplicator.blueInputQueue)"
FORCE_DELETE_GREEN_INPUT_QUEUE="$(get_inception_config_value order-processor-force-delete incoming-orders "$NS" duplicator.greenInputQueue)"
FORCE_DELETE_BLUE_OUTPUT_QUEUE="$(get_inception_config_value order-processor-force-delete outgoing-results "$NS" combiner.blueOutputQueue)"
FORCE_DELETE_GREEN_OUTPUT_QUEUE="$(get_inception_config_value order-processor-force-delete outgoing-results "$NS" combiner.greenOutputQueue)"
wait_deployment_env_pair_values "$HTTP_DEPLOYMENT" "$FORCE_DELETE_DEPLOYMENT" "$NS" INPUT_QUEUE "$FORCE_DELETE_BLUE_INPUT_QUEUE" "$FORCE_DELETE_GREEN_INPUT_QUEUE" orders
wait_deployment_env_pair_values "$HTTP_DEPLOYMENT" "$FORCE_DELETE_DEPLOYMENT" "$NS" OUTPUT_QUEUE "$FORCE_DELETE_BLUE_OUTPUT_QUEUE" "$FORCE_DELETE_GREEN_OUTPUT_QUEUE" results
kubectl rollout status deployment/"$HTTP_DEPLOYMENT" -n "$NS" --timeout=120s
kubectl rollout status deployment/"$FORCE_DELETE_DEPLOYMENT" -n "$NS" --timeout=120s
echo "Publishing force-delete verification message..."
FORCE_DELETE_ORDER_ID="force-delete-$(date +%s)"
publish_rabbitmq_message "orders" "{\"orderId\":\"$FORCE_DELETE_ORDER_ID\",\"type\":\"order\",\"action\":\"force-delete-check\"}"
for i in $(seq 1 30); do
    STATUS_JSON="$(kubectl get bluegreendeployment order-processor-force-delete -n "$NS" -o json)"
    OBSERVED_COUNT="$(printf '%s' "$STATUS_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status", {}).get("testCasesObserved", 0))')"
    PENDING_COUNT="$(printf '%s' "$STATUS_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status", {}).get("testCasesPending", 0))')"
    TRACKED_COUNT="$((OBSERVED_COUNT + PENDING_COUNT))"
    echo "  Force-delete BGD tracked=$TRACKED_COUNT observed=$OBSERVED_COUNT pending=$PENDING_COUNT ($i/30)"
    if [ "$TRACKED_COUNT" -ge 1 ]; then
        break
    fi
    sleep 2
done
if [ "$TRACKED_COUNT" -lt 1 ]; then
    echo "Expected force-delete BGD to register at least one store record before forced deletion" >&2
    kubectl get bluegreendeployment order-processor-force-delete -n "$NS" -o yaml >&2
    exit 1
fi
kubectl patch bluegreendeployment order-processor-force-delete -n "$NS" --type=merge -p '{"metadata":{"finalizers":[]}}'
kubectl delete bluegreendeployment order-processor-force-delete -n "$NS" --wait=false
wait_deleted bluegreendeployment order-processor-force-delete "$NS"
wait_no_blue_green_ref_resources "$NS" order-processor-force-delete
if [ "$E2E_STATE_STORE" = "postgres" ]; then
    FORCE_DELETE_ROWS="$(kubectl exec -n "$NS_SYSTEM" deploy/postgres -- env PGPASSWORD=fluidbg psql -U fluidbg -d fluidbg -tAc "select count(*) from fluidbg_cases where blue_green_ref='order-processor-force-delete';" | tr -d '[:space:]')"
    if [ "$FORCE_DELETE_ROWS" != "0" ]; then
        echo "Expected Postgres store rows for force-deleted BGD to be cleaned, got $FORCE_DELETE_ROWS" >&2
        exit 1
    fi
fi
wait_deployment_label "$HTTP_DEPLOYMENT" "$NS" '{.metadata.labels.fluidbg\.io/green}' true

echo ""
echo "--- Step 24: Final pod state ---"
kubectl get pods -n "$NS"
kubectl get pods -n "$NS_SYSTEM"

echo ""
echo "--- Step 25: Verify Helm uninstall cleanup ---"
helm uninstall fluidbg-e2e -n "$NS_SYSTEM" --wait
wait_deleted deployment fluidbg-operator "$NS_SYSTEM"
wait_deleted deployment fluidbg-rabbitmq-manager "$NS_SYSTEM"
wait_deleted service fluidbg-operator "$NS_SYSTEM"
wait_deleted service fluidbg-rabbitmq-manager "$NS_SYSTEM"
wait_deleted serviceaccount fluidbg-operator "$NS_SYSTEM"
wait_deleted secret fluidbg-e2e-auth "$NS_SYSTEM"
wait_deleted inceptionplugin http "$NS"
wait_deleted inceptionplugin rabbitmq "$NS"
if kubectl get clusterrole fluidbg-operator >/dev/null 2>&1; then
    echo "clusterrole/fluidbg-operator was not removed by Helm uninstall" >&2
    exit 1
fi
if kubectl get clusterrolebinding fluidbg-operator >/dev/null 2>&1; then
    echo "clusterrolebinding/fluidbg-operator was not removed by Helm uninstall" >&2
    exit 1
fi

echo ""
echo "=== E2E Test Complete ==="
