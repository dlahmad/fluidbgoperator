import json
import os
import threading
import time
import uuid

import pika
import requests
from flask import Flask, request, jsonify

app = Flask(__name__)

AMQP_URL = os.environ.get("AMQP_URL", "amqp://fluidbg:fluidbg@rabbitmq.fluidbg-system:5672/")
INPUT_QUEUE = os.environ.get("INPUT_QUEUE", "orders")
OUTPUT_QUEUE = os.environ.get("OUTPUT_QUEUE", "results")
HTTP_UPSTREAM = os.environ.get("HTTP_UPSTREAM", "http://localhost:8081")
PORT = int(os.environ.get("PORT", "8080"))

cases = {}
cases_lock = threading.Lock()


def complete_http_proxy_case_if_ready(case):
    if case.get("output_message_seen") and case.get("http_call_seen"):
        case["status"] = "passed"
        case["error_message"] = None
    else:
        case["status"] = "observing"


def is_http_proxy_message(payload):
    original = payload.get("originalMessage") or {}
    return original.get("action") == "http-proxy-check"


def get_channel():
    params = pika.URLParameters(AMQP_URL)
    connection = pika.BlockingConnection(params)
    channel = connection.channel()
    channel.queue_declare(queue=INPUT_QUEUE, durable=False)
    channel.queue_declare(queue=OUTPUT_QUEUE, durable=False)
    channel.queue_declare(queue="orders-green", durable=False)
    channel.queue_declare(queue="orders-blue", durable=False)
    return connection, channel


def publish_json(queue, payload):
    connection, channel = get_channel()
    try:
        channel.confirm_delivery()
        channel.basic_publish("", queue, json.dumps(payload))
    finally:
        connection.close()


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
        publish_json(INPUT_QUEUE, msg)
    except Exception as e:
        return jsonify({"testId": test_id, "status": "triggered", "publish_error": str(e)})
    return jsonify({"testId": test_id, "status": "triggered"})


@app.route("/observe/<test_id>/<inception_point>", methods=["POST"])
def observe(test_id, inception_point):
    data = request.get_json(force=True, silent=True) or {}
    with cases_lock:
        if test_id not in cases:
            cases[test_id] = {"status": "observing"}
        current_status = cases[test_id].get("status")
        if current_status in ("passed", "failed"):
            return jsonify({"testId": test_id, "status": current_status})
        case = cases[test_id]
        case["observation"] = data
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
            if case["http_call_seen"] and is_http_proxy_message(result_message):
                complete_http_proxy_case_if_ready(case)
            else:
                case["status"] = "observing"
        else:
            case["status"] = "observing"
    return jsonify({"testId": test_id, "status": "observing"})


@app.route("/result/<test_id>", methods=["GET"])
def result(test_id):
    with cases_lock:
        case = cases.get(test_id, {})
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
    app.run(host="0.0.0.0", port=PORT)
