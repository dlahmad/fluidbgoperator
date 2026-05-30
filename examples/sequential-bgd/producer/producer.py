import json
import os
import socket
import time
import asyncio

import nats
from nats.js.errors import NotFoundError, NoStreamResponseError
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
        js = nc.jetstream()
        await ensure_nats_stream(js, OUTPUT_SUBJECT)
        order_id = f"demo-{INSTANCE}-{counter}"
        payload = {
            "orderId": order_id,
            "type": "demo-order",
            "producer": INSTANCE,
            "sequence": counter,
        }
        await js.publish(OUTPUT_SUBJECT, json.dumps(payload).encode())
        print(f"published {order_id}", flush=True)
    finally:
        await nc.close()


def nats_stream_name(subject):
    hash_value = 5381
    for byte in subject.encode():
        hash_value = ((hash_value * 33) + byte) & 0xFFFFFFFFFFFFFFFF
    hint = "".join(
        char for char in subject if char.isascii() and (char.isalnum() or char in "-_")
    )[:24]
    return f"fbg_{hint}_{hash_value:016x}"


async def ensure_nats_stream(js, subject):
    name = nats_stream_name(subject)
    try:
        await js.stream_info(name)
    except (NotFoundError, NoStreamResponseError):
        try:
            await js.add_stream(name=name, subjects=[subject])
        except Exception:
            await js.stream_info(name)
    return name


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
