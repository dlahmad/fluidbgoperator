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
            "httpByPrefix": {},
            "httpEvents": [],
            "output": None,
            "outputByPrefix": {},
            "outputEvents": [],
            "complete": False,
            "completeByPrefix": {},
        },
    )


def recompute(record):
    prefixes = set(record.get("httpByPrefix", {}).keys()) | set(
        record.get("outputByPrefix", {}).keys()
    )
    record["completeByPrefix"] = {
        prefix: complete_for_prefix(record, prefix) for prefix in sorted(prefixes)
    }
    record["complete"] = any(record["completeByPrefix"].values())


def complete_for_prefix(record, prefix):
    return bool(
        prefix
        and record.get("httpByPrefix", {}).get(prefix)
        and record.get("outputByPrefix", {}).get(prefix)
    )


def sequences_for_prefix(prefix, field):
    seqs = {
        record["sequence"]
        for record in cases.values()
        if record.get(field)
        and record.get("sequence") is not None
        and record_has_prefix(record, prefix, field)
    }
    return sorted(seqs)


def output_sequences_all_prefixes():
    return sorted(
        {
            record["sequence"]
            for record in cases.values()
            if record.get("output")
            and record.get("sequence") is not None
        }
    )


def record_has_prefix(record, prefix, field):
    if not prefix:
        return False
    if field == "http":
        return bool(record.get("httpByPrefix", {}).get(prefix))
    if field == "output":
        return bool(record.get("outputByPrefix", {}).get(prefix))
    if field == "complete":
        return complete_for_prefix(record, prefix)
    return False


def missing_sequences(seqs):
    if not seqs:
        return []
    low, high = min(seqs), max(seqs)
    return [seq for seq in range(low, high + 1) if seq not in seqs]


def print_gap_summary(prefix):
    all_output_missing = missing_sequences(output_sequences_all_prefixes())
    complete = len(sequences_for_prefix(prefix, "complete"))
    if all_output_missing:
        print(
            "OUTPUT STREAM GAP "
            f"allOutputMissing={all_output_missing} prefix={prefix} "
            f"candidateCompleteCount={complete}",
            flush=True,
        )
    else:
        print(
            "OUTPUT STREAM OK "
            f"allOutputMissing=[] prefix={prefix} candidateCompleteCount={complete}",
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
        record["httpEvents"].append(payload)
        record["httpByPrefix"][prefix] = payload
        recompute(record)
        print(
            f"HTTP sequence={record['sequence']} order={order_id} prefix={prefix} complete={complete_for_prefix(record, prefix)}",
            flush=True,
        )
        print_gap_summary(prefix)
    return jsonify({"ok": True})


@app.get("/cases/<order_id>")
def get_case(order_id):
    expected_prefix = request.args.get("expectedPrefix", "")
    with lock:
        record = dict(cases.get(order_id, {"orderId": order_id, "complete": False}))
        http_by_prefix = dict(record.get("httpByPrefix") or {})
        output_by_prefix = dict(record.get("outputByPrefix") or {})
        expected_http = http_by_prefix.get(expected_prefix)
        expected_output = output_by_prefix.get(expected_prefix)
        output = record.get("output") or {}
        record["expectedPrefix"] = expected_prefix
        record["expectedHttpSeen"] = bool(expected_http)
        record["expectedOutputSeen"] = bool(expected_output)
        record["expectedPrefixMatched"] = bool(expected_http and expected_output)
        record["expectedHttp"] = expected_http
        record["expectedOutput"] = expected_output
        if expected_http:
            record["http"] = expected_http
        if expected_output:
            record["output"] = expected_output
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


@app.get("/summary")
def summary():
    with lock:
        outputs_by_prefix = {}
        complete_by_prefix = {}
        for record in cases.values():
            for prefix in record.get("outputByPrefix", {}).keys():
                outputs_by_prefix[prefix] = outputs_by_prefix.get(prefix, 0) + 1
            for prefix, complete in record.get("completeByPrefix", {}).items():
                if complete:
                    complete_by_prefix[prefix] = complete_by_prefix.get(prefix, 0) + 1
        output_sequences = output_sequences_all_prefixes()
        return jsonify(
            {
                "outputCount": len(output_sequences),
                "firstOutputSequence": min(output_sequences) if output_sequences else None,
                "lastOutputSequence": max(output_sequences) if output_sequences else None,
                "allOutputMissing": missing_sequences(output_sequences),
                "outputsByPrefix": outputs_by_prefix,
                "completeByPrefix": complete_by_prefix,
            }
        )


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
                        record["outputEvents"].append(payload)
                        record["outputByPrefix"][prefix] = payload
                        recompute(record)
                        print(
                            f"OUTPUT sequence={record['sequence']} order={order_id} result={payload.get('result')} complete={complete_for_prefix(record, prefix)}",
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
