import json
import os
import socket
import time
import asyncio

import nats
import pika

TRANSPORT = os.environ.get("TRANSPORT", "rabbitmq")
AMQP_URL = os.environ.get("AMQP_URL", "amqp://fluidbg:fluidbg@rabbitmq:5672/")
NATS_URL = os.environ.get("NATS_URL", "nats://nats:4222")
OUTPUT_QUEUE = os.environ.get("OUTPUT_QUEUE", "orders")
OUTPUT_SUBJECT = os.environ.get("OUTPUT_SUBJECT", OUTPUT_QUEUE)
INTERVAL_SECONDS = float(os.environ.get("INTERVAL_SECONDS", "2"))
INSTANCE = socket.gethostname()


def publish(counter):
    connection = pika.BlockingConnection(pika.URLParameters(AMQP_URL))
    try:
        ch = connection.channel()
        ch.queue_declare(queue=OUTPUT_QUEUE, durable=True)
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


async def publish_nats(counter):
    nc = await nats.connect(NATS_URL)
    try:
        order_id = f"demo-{INSTANCE}-{counter}"
        payload = {
            "orderId": order_id,
            "type": "demo-order",
            "producer": INSTANCE,
            "sequence": counter,
        }
        await nc.publish(OUTPUT_SUBJECT, json.dumps(payload).encode())
        await nc.flush()
        print(f"published {order_id}", flush=True)
    finally:
        await nc.close()


counter = 0
while True:
    try:
        counter += 1
        if TRANSPORT == "nats":
            asyncio.run(publish_nats(counter))
        else:
            publish(counter)
        time.sleep(INTERVAL_SECONDS)
    except Exception as exc:
        print(f"producer error: {exc}; retrying", flush=True)
        time.sleep(3)
