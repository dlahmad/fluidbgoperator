import json
import os
import threading
import time
import uuid
import asyncio

import nats
import pika
import requests
from flask import Flask, request, jsonify

app = Flask(__name__)

TRANSPORT = os.environ.get("TRANSPORT", "rabbitmq")
AMQP_URL = os.environ.get("AMQP_URL", "amqp://fluidbg:fluidbg@rabbitmq.fluidbg-system:5672/")
NATS_URL = os.environ.get("NATS_URL", "nats://nats.fluidbg-system:4222")
INPUT_QUEUE = os.environ.get("INPUT_QUEUE", "orders")
OUTPUT_QUEUE = os.environ.get("OUTPUT_QUEUE", "results")
INPUT_SUBJECT = os.environ.get("INPUT_SUBJECT", INPUT_QUEUE)
HTTP_UPSTREAM = os.environ.get("HTTP_UPSTREAM", "http://localhost:8081")
PORT = int(os.environ.get("PORT", "8080"))
STARTUP_DELAY_SECONDS = int(os.environ.get("STARTUP_DELAY_SECONDS", "0"))
VERIFIER_AUTH_TOKENS = json.loads(os.environ.get("FLUIDBG_VERIFIER_AUTH_TOKENS_JSON", "{}") or "{}")

cases = {}
cases_lock = threading.Lock()


def bearer_token():
    header = request.headers.get("Authorization", "")
    if header.startswith("Bearer "):
        return header[len("Bearer "):]
    return None


def authorized_for_inception(inception_point):
    expected = VERIFIER_AUTH_TOKENS.get(inception_point)
    return bool(expected) and bearer_token() == expected


def authorized_for_any_inception():
    token = bearer_token()
    return bool(token) and token in set(VERIFIER_AUTH_TOKENS.values())


def complete_http_proxy_case_if_ready(case):
    if case.get("output_message_seen") and case.get("http_call_seen"):
        case["status"] = "passed"
        case["error_message"] = None
    else:
        case["status"] = "observing"


def complete_http_plugin_case_if_ready(case, test_id):
    if test_id.startswith("http-direct-proxy-") and case.get("http_call_seen"):
        case["status"] = "passed"
        case["error_message"] = None
        return True
    if (
        test_id.startswith("http-mock-")
        and case.get("observation_seen")
        and case.get("mock_call_seen")
    ):
        case["status"] = "passed"
        case["error_message"] = None
        return True
    return False


def complete_delayed_case_if_ready(case):
    ready_at = case.get("verify_ready_at")
    if ready_at is not None and time.time() < ready_at:
        case["status"] = "observing"
        return False
    if ready_at is not None:
        case.pop("verify_ready_at", None)
        case["status"] = "passed"
        case["error_message"] = None
        return True
    return False


def is_http_proxy_message(payload):
    original = payload.get("originalMessage") or {}
    return original.get("action") == "http-proxy-check"


def get_channel():
    params = pika.URLParameters(AMQP_URL)
    connection = pika.BlockingConnection(params)
    channel = connection.channel()
    channel.queue_declare(queue=INPUT_QUEUE, durable=True)
    channel.queue_declare(queue=OUTPUT_QUEUE, durable=True)
    channel.queue_declare(queue="orders-green", durable=True)
    channel.queue_declare(queue="orders-blue", durable=True)
    return connection, channel


def publish_json(queue, payload):
    connection, channel = get_channel()
    try:
        channel.confirm_delivery()
        channel.basic_publish("", queue, json.dumps(payload))
    finally:
        connection.close()


def publish_transport(payload):
    if TRANSPORT == "nats":
        async def publish():
            nc = await nats.connect(NATS_URL)
            try:
                await nc.publish(INPUT_SUBJECT, json.dumps(payload).encode())
                await nc.flush()
            finally:
                await nc.close()

        asyncio.run(publish())
    else:
        publish_json(INPUT_QUEUE, payload)


# ── Flask endpoints ──────────────────────────────────────────────────

@app.route("/health", methods=["GET"])
def health():
    return "ok"


@app.route("/trigger", methods=["POST"])
def trigger():
    data = request.get_json(force=True, silent=True) or {}
    test_id = data.get("testId") or data.get("test_id") or str(uuid.uuid4())[:8]
    with cases_lock:
        cases[test_id] = {"status": "triggered", "payload": data}
    # publish a test message to the input queue so the blue app processes it
    msg = {"orderId": test_id, "type": "order", "action": "process"}
    try:
        publish_transport(msg)
    except Exception:
        app.logger.exception("failed to publish trigger message")
        return jsonify({"testId": test_id, "status": "triggered", "publish_error": "publish failed"}), 502
    return jsonify({"testId": test_id, "status": "triggered"})


@app.route("/observe/<test_id>/<inception_point>", methods=["POST"])
def observe(test_id, inception_point):
    if not authorized_for_inception(inception_point):
        return jsonify({"error": "unauthorized"}), 401
    data = request.get_json(force=True, silent=True) or {}
    with cases_lock:
        if test_id not in cases:
            cases[test_id] = {"status": "observing"}
        current_status = cases[test_id].get("status")
        if current_status in ("passed", "failed"):
            return jsonify({"testId": test_id, "status": current_status})
        case = cases[test_id]
        case["observation"] = data
        case["observation_seen"] = True
        if inception_point == "outgoing-results":
            payload = data.get("payload") or {}
            original = payload.get("originalMessage") or {}
            route = data.get("route")
            case["result_message"] = payload
            if is_http_proxy_message(payload):
                case["output_message_seen"] = case.get("output_message_seen") or route == "blue"
            if route != "blue":
                case["status"] = "observing"
            elif is_http_proxy_message(payload):
                complete_http_proxy_case_if_ready(case)
            elif original.get("shouldPass", True):
                delay = int(original.get("verifyDelaySeconds", 0) or 0)
                if delay > 0:
                    if "verify_ready_at" not in case:
                        case["verify_ready_at"] = time.time() + delay
                    if not complete_delayed_case_if_ready(case):
                        case["status"] = "observing"
                else:
                    case["status"] = "passed"
                    case["error_message"] = None
            else:
                case["status"] = "failed"
                case["error_message"] = original.get(
                    "failureReason", "candidate verification failed"
                )
        elif inception_point == "http-upstream":
            payload = data.get("payload") or {}
            route = data.get("route")
            case["http_call_seen"] = case.get("http_call_seen") or (
                route == "blue"
                and payload.get("action") == "http-proxy-check"
                and payload.get("orderId") == test_id
            )
            result_message = case.get("result_message") or {}
            if complete_http_plugin_case_if_ready(case, test_id):
                pass
            elif case["http_call_seen"] and is_http_proxy_message(result_message):
                complete_http_proxy_case_if_ready(case)
            else:
                case["status"] = "observing"
        else:
            case["status"] = "observing"
    return jsonify({"testId": test_id, "status": "observing"})


@app.route("/mock/<test_id>/<inception_point>", methods=["POST", "PUT", "PATCH", "GET"])
def mock_response(test_id, inception_point):
    if not authorized_for_inception(inception_point):
        return jsonify({"error": "unauthorized"}), 401
    payload = request.get_json(force=True, silent=True) or {}
    with cases_lock:
        if test_id not in cases:
            cases[test_id] = {"status": "observing"}
        case = cases[test_id]
        case["mock_call_seen"] = True
        case["mock_payload"] = payload
        case["mock_headers"] = {
            "x-fluidbg-test-id": request.headers.get("x-fluidbg-test-id"),
            "x-fluidbg-inception-point": request.headers.get("x-fluidbg-inception-point"),
            "x-fluidbg-route": request.headers.get("x-fluidbg-route"),
        }
        complete_http_plugin_case_if_ready(case, test_id)
    return jsonify({
        "mocked": True,
        "testId": test_id,
        "inceptionPoint": inception_point,
        "payload": payload,
    }), 209, {"X-FluidBG-Mock": "verifier"}


@app.route("/result/<test_id>", methods=["GET"])
def result(test_id):
    if not authorized_for_any_inception():
        return jsonify({"error": "unauthorized"}), 401
    with cases_lock:
        case = cases.get(test_id, {})
        if case:
            complete_delayed_case_if_ready(case)
    status = case.get("status", "pending")
    if status == "passed":
        return jsonify({"passed": True, "testId": test_id, "errorMessage": None})
    elif status == "failed":
        return jsonify({
            "passed": False,
            "testId": test_id,
            "errorMessage": case.get("error_message", "verification failed"),
        })
    return jsonify({
        "passed": None,
        "testId": test_id,
        "status": status,
        "errorMessage": None,
    })


@app.route("/cases", methods=["GET"])
def list_cases():
    with cases_lock:
        return jsonify(dict(cases))


# ── Start ────────────────────────────────────────────────────────────

if __name__ == "__main__":
    if STARTUP_DELAY_SECONDS > 0:
        time.sleep(STARTUP_DELAY_SECONDS)
    app.run(host="0.0.0.0", port=PORT)
