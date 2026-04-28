import json
import os
import threading
import time

import pika
from flask import Flask, jsonify, request

app = Flask(__name__)

AMQP_URL = os.environ.get("AMQP_URL", "amqp://fluidbg:fluidbg@rabbitmq:5672/")
INPUT_QUEUE = os.environ.get("INPUT_QUEUE", "results")
PORT = int(os.environ.get("PORT", "8080"))

cases = {}
lock = threading.Lock()


def case(order_id):
    return cases.setdefault(
        order_id,
        {
            "orderId": order_id,
            "sequence": None,
            "http": None,
            "output": None,
            "complete": False,
        },
    )


def recompute(record):
    record["complete"] = bool(record["http"] and record["output"])


def sequences_for_prefix(prefix, field):
    seqs = {
        record["sequence"]
        for record in cases.values()
        if record.get(field)
        and record.get("sequence") is not None
        and record_matches_prefix(record, prefix)
    }
    return sorted(seqs)


def record_matches_prefix(record, prefix):
    http_prefix = str((record.get("http") or {}).get("outputPrefix", ""))
    output_result = str((record.get("output") or {}).get("result", ""))
    return http_prefix == prefix or output_result.startswith(f"{prefix}-")


def missing_sequences(seqs):
    if not seqs:
        return []
    low, high = min(seqs), max(seqs)
    return [seq for seq in range(low, high + 1) if seq not in seqs]


def print_gap_summary(prefix):
    http_missing = missing_sequences(sequences_for_prefix(prefix, "http"))
    output_missing = missing_sequences(sequences_for_prefix(prefix, "output"))
    complete_missing = missing_sequences(sequences_for_prefix(prefix, "complete"))
    if http_missing or output_missing or complete_missing:
        print(
            "SEQUENCE GAP "
            f"prefix={prefix} httpMissing={http_missing} "
            f"outputMissing={output_missing} completeMissing={complete_missing}",
            flush=True,
        )
    else:
        print(
            "SEQUENCE OK "
            f"prefix={prefix} httpMissing=[] outputMissing=[] completeMissing=[]",
            flush=True,
        )


@app.get("/health")
def health():
    return "ok"


@app.post("/audit")
def audit():
    payload = request.get_json(force=True, silent=True) or {}
    order_id = payload.get("orderId", "unknown")
    prefix = payload.get("outputPrefix", "unknown")
    with lock:
        record = case(order_id)
        record["sequence"] = payload.get("sequence", record.get("sequence"))
        record["http"] = payload
        recompute(record)
        print(
            f"HTTP sequence={record['sequence']} order={order_id} prefix={prefix} complete={record['complete']}",
            flush=True,
        )
        print_gap_summary(prefix)
    return jsonify({"ok": True})


@app.get("/cases/<order_id>")
def get_case(order_id):
    expected_prefix = request.args.get("expectedPrefix", "")
    with lock:
        record = dict(cases.get(order_id, {"orderId": order_id, "complete": False}))
        output = record.get("output") or {}
        record["expectedPrefix"] = expected_prefix
        record["expectedPrefixMatched"] = (
            bool(expected_prefix)
            and str(output.get("result", "")).startswith(f"{expected_prefix}-")
        )
        record["missingHttpSequences"] = (
            missing_sequences(sequences_for_prefix(expected_prefix, "http"))
            if expected_prefix
            else []
        )
        record["missingOutputSequences"] = (
            missing_sequences(sequences_for_prefix(expected_prefix, "output"))
            if expected_prefix
            else []
        )
        record["missingCompleteSequences"] = (
            missing_sequences(sequences_for_prefix(expected_prefix, "complete"))
            if expected_prefix
            else []
        )
    return jsonify(record)


@app.get("/cases")
def list_cases():
    with lock:
        return jsonify(cases)


def consume_results():
    while True:
        try:
            connection = pika.BlockingConnection(pika.URLParameters(AMQP_URL))
            ch = connection.channel()
            ch.queue_declare(queue=INPUT_QUEUE, durable=False)
            ch.basic_qos(prefetch_count=1)
            print(
                "sink acts as normal downstream demo service: "
                f"HTTP /audit plus queue consumer for {INPUT_QUEUE}",
                flush=True,
            )

            def handle(ch, method, _properties, body):
                try:
                    payload = json.loads(body)
                    order_id = payload.get("orderId", "unknown")
                    prefix = str(payload.get("result", "")).split("-", 1)[0]
                    with lock:
                        record = case(order_id)
                        record["sequence"] = payload.get("sequence", record.get("sequence"))
                        record["output"] = payload
                        recompute(record)
                        print(
                            f"OUTPUT sequence={record['sequence']} order={order_id} result={payload.get('result')} complete={record['complete']}",
                            flush=True,
                        )
                        print_gap_summary(prefix)
                    ch.basic_ack(method.delivery_tag)
                except Exception as exc:
                    print(f"sink output consume failed: {exc}", flush=True)
                    ch.basic_nack(method.delivery_tag, requeue=True)

            ch.basic_consume(INPUT_QUEUE, handle, auto_ack=False)
            print(f"sink consuming output queue={INPUT_QUEUE}", flush=True)
            ch.start_consuming()
        except Exception as exc:
            print(f"sink consumer error: {exc}; retrying", flush=True)
            time.sleep(3)


threading.Thread(target=consume_results, daemon=True).start()

if __name__ == "__main__":
    app.run(host="0.0.0.0", port=PORT)
