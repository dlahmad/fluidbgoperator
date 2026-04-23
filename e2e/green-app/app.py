import json
import os
import time

import pika
import requests

AMQP_URL = os.environ.get("AMQP_URL", "amqp://fluidbg:fluidbg@rabbitmq.fluidbg-system:5672/")
INPUT_QUEUE = os.environ.get("INPUT_QUEUE", "orders")
OUTPUT_QUEUE = os.environ.get("OUTPUT_QUEUE", "results")
HTTP_UPSTREAM = os.environ.get("HTTP_UPSTREAM", "http://httpbin.org/post")


def get_channel():
    params = pika.URLParameters(AMQP_URL)
    connection = pika.BlockingConnection(params)
    channel = connection.channel()
    channel.queue_declare(queue=INPUT_QUEUE, durable=False)
    channel.queue_declare(queue=OUTPUT_QUEUE, durable=False)
    return connection, channel


def process_message(ch, method, properties, body):
    try:
        msg = json.loads(body)
        order_id = msg.get("orderId", "unknown")

        try:
            resp = requests.post(HTTP_UPSTREAM, json=msg, timeout=5)
            http_status = resp.status_code
        except Exception:
            http_status = 0

        result = {
            "orderId": order_id,
            "httpStatus": http_status,
            "originalMessage": msg,
            "processedBy": "green",
        }
        _, out_ch = get_channel()
        out_ch.basic_publish("", OUTPUT_QUEUE, json.dumps(result))

        ch.basic_ack(method.delivery_tag)
    except Exception:
        ch.basic_ack(method.delivery_tag)


def main():
    while True:
        try:
            conn, ch = get_channel()
            ch.basic_qos(prefetch_count=1)
            ch.basic_consume(INPUT_QUEUE, process_message, auto_ack=False)
            print(f"green-app consuming from {INPUT_QUEUE}", flush=True)
            ch.start_consuming()
        except Exception as e:
            print(f"green-app error: {e}, reconnecting...", flush=True)
            time.sleep(3)


if __name__ == "__main__":
    main()
