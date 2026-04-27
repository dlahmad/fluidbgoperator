import json
import os
import time

import pika
import requests

AMQP_URL = os.environ.get("AMQP_URL", "amqp://fluidbg:fluidbg@rabbitmq.fluidbg-system:5672/")
INPUT_QUEUE = os.environ.get("INPUT_QUEUE", "orders")
OUTPUT_QUEUE = os.environ.get("OUTPUT_QUEUE", "results")
HTTP_UPSTREAM = os.environ.get("HTTP_UPSTREAM", "http://httpbin.org/post")
PORT = int(os.environ.get("PORT", "8081"))
INSTANCE_NAME = os.environ.get("HOSTNAME", "unknown")
TEMP_QUEUE_DURABLE = os.environ.get("AMQP_TEMP_QUEUE_DURABLE", "false").lower() == "true"
TEMP_QUEUE_ARGUMENTS = json.loads(os.environ.get("AMQP_TEMP_QUEUE_ARGUMENTS_JSON", "{}"))


def queue_declaration(queue):
    if queue.startswith("fluidbg-"):
        return {"durable": TEMP_QUEUE_DURABLE, "arguments": TEMP_QUEUE_ARGUMENTS}
    return {"durable": False, "arguments": None}


def get_channel():
    params = pika.URLParameters(AMQP_URL)
    connection = pika.BlockingConnection(params)
    channel = connection.channel()
    channel.queue_declare(queue=INPUT_QUEUE, **queue_declaration(INPUT_QUEUE))
    channel.queue_declare(queue=OUTPUT_QUEUE, **queue_declaration(OUTPUT_QUEUE))
    return connection, channel


def publish_json(queue, payload):
    connection, channel = get_channel()
    try:
        channel.confirm_delivery()
        channel.basic_publish("", queue, json.dumps(payload))
    finally:
        connection.close()


def call_http_upstream_if_required(msg):
    if msg.get("action") != "http-proxy-check":
        return 204
    try:
        resp = requests.post(HTTP_UPSTREAM, json=msg, timeout=5)
        return resp.status_code
    except Exception:
        return 0


def process_message(ch, method, properties, body):
    try:
        msg = json.loads(body)
        order_id = msg.get("orderId", "unknown")

        http_status = call_http_upstream_if_required(msg)

        # write result to output queue
        result = {
            "orderId": order_id,
            "httpStatus": http_status,
            "originalMessage": msg,
            "processedBy": "blue",
            "instanceName": INSTANCE_NAME,
        }
        publish_json(OUTPUT_QUEUE, result)

        ch.basic_ack(method.delivery_tag)
    except Exception as e:
        print(f"blue-app failed to process message: {e}", flush=True)
        ch.basic_nack(method.delivery_tag, requeue=True)


def main():
    while True:
        try:
            conn, ch = get_channel()
            ch.basic_qos(prefetch_count=1)
            ch.basic_consume(INPUT_QUEUE, process_message, auto_ack=False)
            print(f"blue-app consuming from {INPUT_QUEUE}", flush=True)
            ch.start_consuming()
        except Exception as e:
            print(f"blue-app error: {e}, reconnecting...", flush=True)
            time.sleep(3)


if __name__ == "__main__":
    main()
