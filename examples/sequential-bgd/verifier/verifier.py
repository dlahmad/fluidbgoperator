import os
import threading

import requests
from flask import Flask, jsonify, request

app = Flask(__name__)

EXPECTED_PREFIX = os.environ.get("EXPECTED_PREFIX", "v2")
SINK_URL = os.environ.get("SINK_URL", "http://order-flow-sink:8080")
PORT = int(os.environ.get("PORT", "8080"))

cases = {}
lock = threading.Lock()


def case(test_id):
    return cases.setdefault(
        test_id,
        {
            "status": "observing",
            "events": [],
        },
    )


@app.get("/health")
def health():
    return "ok"


@app.post("/observe/<test_id>/<inception_point>")
def observe(test_id, inception_point):
    data = request.get_json(force=True, silent=True) or {}
    with lock:
        record = case(test_id)
        record["events"].append(
            {
                "kind": "testcase-observation",
                "inceptionPoint": inception_point,
                "route": data.get("route"),
                "payload": data.get("payload") or {},
            }
        )
        print(
            f"testcase order={test_id} point={inception_point} route={data.get('route')}",
            flush=True,
        )
    return jsonify({"ok": True})


@app.get("/result/<test_id>")
def result(test_id):
    try:
        sink_response = requests.get(
            f"{SINK_URL.rstrip('/')}/cases/{test_id}",
            params={"expectedPrefix": EXPECTED_PREFIX},
            timeout=3,
        )
        sink_response.raise_for_status()
        sink_case = sink_response.json()
    except Exception as exc:
        return jsonify(
            {
                "passed": None,
                "testId": test_id,
                "status": "sink-unavailable",
                "errorMessage": str(exc),
            }
        )

    output = sink_case.get("output") or {}
    http_seen = bool(sink_case.get("http"))
    output_seen = bool(output)
    prefix_ok = bool(sink_case.get("expectedPrefixMatched"))
    passed = http_seen and output_seen and prefix_ok
    with lock:
        record = case(test_id)
        record["status"] = "passed" if passed else "observing"
        record["sink"] = sink_case
    if passed:
        return jsonify(
            {
                "passed": True,
                "testId": test_id,
                "errorMessage": None,
                "sink": sink_case,
            }
        )
    return jsonify(
        {
            "passed": None,
            "testId": test_id,
            "status": "observing",
            "httpSeen": http_seen,
            "outputSeen": output_seen,
            "prefixOk": prefix_ok,
            "sink": sink_case,
            "errorMessage": None,
        }
    )


@app.get("/cases")
def list_cases():
    with lock:
        return jsonify(cases)


if __name__ == "__main__":
    app.run(host="0.0.0.0", port=PORT)
