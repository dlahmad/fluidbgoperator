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


def get_channel():
    params = pika.URLParameters(AMQP_URL)
    connection = pika.BlockingConnection(params)
    channel = connection.channel()
    channel.queue_declare(queue=INPUT_QUEUE, durable=False)
    channel.queue_declare(queue=OUTPUT_QUEUE, durable=False)
    channel.queue_declare(queue="orders-green", durable=False)
    channel.queue_declare(queue="orders-blue", durable=False)
    return connection, channel


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
        _, ch = get_channel()
        ch.basic_publish("", INPUT_QUEUE, json.dumps(msg))
    except Exception as e:
        return jsonify({"testId": test_id, "status": "triggered", "publish_error": str(e)})
    return jsonify({"testId": test_id, "status": "triggered"})


@app.route("/observe/<test_id>/<inception_point>", methods=["POST"])
def observe(test_id, inception_point):
    data = request.get_json(force=True, silent=True) or {}
    with cases_lock:
        if test_id not in cases:
            cases[test_id] = {"status": "observing"}
        cases[test_id]["observation"] = data
        if inception_point == "outgoing-results":
            cases[test_id]["status"] = "passed"
            cases[test_id]["result_message"] = data.get("payload")
        else:
            cases[test_id]["status"] = "observing"
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
