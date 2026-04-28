import json
import os
import socket
import time

import pika

AMQP_URL = os.environ.get("AMQP_URL", "amqp://fluidbg:fluidbg@rabbitmq:5672/")
OUTPUT_QUEUE = os.environ.get("OUTPUT_QUEUE", "orders")
INTERVAL_SECONDS = float(os.environ.get("INTERVAL_SECONDS", "2"))
INSTANCE = socket.gethostname()


def publish(counter):
    connection = pika.BlockingConnection(pika.URLParameters(AMQP_URL))
    try:
        ch = connection.channel()
        ch.queue_declare(queue=OUTPUT_QUEUE, durable=False)
        ch.confirm_delivery()
        order_id = f"demo-{INSTANCE}-{counter}"
        payload = {
            "orderId": order_id,
            "type": "demo-order",
            "producer": INSTANCE,
            "sequence": counter,
        }
        ch.basic_publish("", OUTPUT_QUEUE, json.dumps(payload))
        print(f"published {order_id}", flush=True)
    finally:
        connection.close()


counter = 0
while True:
    try:
        counter += 1
        publish(counter)
        time.sleep(INTERVAL_SECONDS)
    except Exception as exc:
        print(f"producer error: {exc}; retrying", flush=True)
        time.sleep(3)
