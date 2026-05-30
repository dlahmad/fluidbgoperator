import json
import os
import time
import asyncio

import nats
from nats.errors import TimeoutError
from nats.js.errors import FetchTimeoutError, NotFoundError, NoStreamResponseError
import pika
import requests

TRANSPORT = os.environ.get("TRANSPORT", "rabbitmq")
AMQP_URL = os.environ.get("AMQP_URL", "amqp://fluidbg:fluidbg@rabbitmq:5672/")
NATS_URL = os.environ.get("NATS_URL", "nats://nats:4222")
INPUT_QUEUE = os.environ.get("INPUT_QUEUE", "orders")
OUTPUT_QUEUE = os.environ.get("OUTPUT_QUEUE", "results")
INPUT_SUBJECT = os.environ.get("INPUT_SUBJECT", INPUT_QUEUE)
OUTPUT_SUBJECT = os.environ.get("OUTPUT_SUBJECT", OUTPUT_QUEUE)
NATS_QUEUE_GROUP = os.environ.get("NATS_QUEUE_GROUP", "order-flow")
OUTPUT_PREFIX = os.environ.get("OUTPUT_PREFIX", "v1")
HTTP_UPSTREAM = os.environ.get("HTTP_UPSTREAM", "")


def channel():
    connection = pika.BlockingConnection(pika.URLParameters(AMQP_URL))
    ch = connection.channel()
    ch.queue_declare(queue=INPUT_QUEUE, durable=True)
    ch.queue_declare(queue=OUTPUT_QUEUE, durable=True)
    return connection, ch


def publish_result(payload):
    connection, ch = channel()
    try:
        ch.confirm_delivery()
        ch.basic_publish("", OUTPUT_QUEUE, json.dumps(payload))
    finally:
        connection.close()


def audit_order(order):
    if not HTTP_UPSTREAM:
        return 0
    audit_payload = {
        "orderId": order["orderId"],
        "sequence": order.get("sequence"),
        "type": "demo-order",
        "outputPrefix": OUTPUT_PREFIX,
        "source": "order-app",
    }
    response = requests.post(
        f"{HTTP_UPSTREAM.rstrip('/')}/audit",
        json=audit_payload,
        timeout=5,
    )
    return response.status_code


def handle_message(ch, method, _properties, body):
    try:
        order = json.loads(body)
        order_id = order.get("orderId")
        if not order_id:
            ch.basic_ack(method.delivery_tag)
            return

        status = audit_order(order)
        result = {
            "orderId": order_id,
            "sequence": order.get("sequence"),
            "type": "demo-result",
            "result": f"{OUTPUT_PREFIX}-{order_id}",
            "httpStatus": status,
        }
        publish_result(result)
        print(f"processed order={order_id} result={result['result']} http={status}", flush=True)
        ch.basic_ack(method.delivery_tag)
    except Exception as exc:
        print(f"processing failed: {exc}", flush=True)
        ch.basic_nack(method.delivery_tag, requeue=True)


def nats_stream_name(subject):
    hash_value = 5381
    for byte in subject.encode():
        hash_value = ((hash_value * 33) + byte) & 0xFFFFFFFFFFFFFFFF
    hint = "".join(
        char for char in subject if char.isascii() and (char.isalnum() or char in "-_")
    )[:24]
    return f"fbg_{hint}_{hash_value:016x}"


def nats_durable_name(queue_group, subject):
    return nats_stream_name(f"{queue_group}_{subject}")


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


async def publish_nats_result(js, payload):
    await ensure_nats_stream(js, OUTPUT_SUBJECT)
    await js.publish(OUTPUT_SUBJECT, json.dumps(payload).encode())


async def handle_nats_message(js, msg):
    try:
        order = json.loads(msg.data.decode())
        order_id = order.get("orderId")
        if not order_id:
            await msg.ack()
            return

        status = audit_order(order)
        result = {
            "orderId": order_id,
            "sequence": order.get("sequence"),
            "type": "demo-result",
            "result": f"{OUTPUT_PREFIX}-{order_id}",
            "httpStatus": status,
        }
        await publish_nats_result(js, result)
        await msg.ack()
        print(f"processed order={order_id} result={result['result']} http={status}", flush=True)
    except Exception as exc:
        print(f"nats processing failed: {exc}", flush=True)
        await msg.nak()


async def nats_main():
    while True:
        try:
            nc = await nats.connect(NATS_URL)
            js = nc.jetstream()
            stream = await ensure_nats_stream(js, INPUT_SUBJECT)
            durable = nats_durable_name(NATS_QUEUE_GROUP, INPUT_SUBJECT)
            subscription = await js.pull_subscribe(
                INPUT_SUBJECT,
                durable=durable,
                stream=stream,
            )
            print(
                f"consuming subject={INPUT_SUBJECT} queueGroup={NATS_QUEUE_GROUP} prefix={OUTPUT_PREFIX}",
                flush=True,
            )
            while True:
                try:
                    messages = await subscription.fetch(1, timeout=1)
                except (FetchTimeoutError, TimeoutError):
                    continue
                for msg in messages:
                    await handle_nats_message(js, msg)
        except Exception as exc:
            print(f"nats consumer error: {exc}; retrying", flush=True)
            await asyncio.sleep(3)


def rabbitmq_main():
    while True:
        try:
            connection, ch = channel()
            ch.basic_qos(prefetch_count=1)
            ch.basic_consume(INPUT_QUEUE, handle_message, auto_ack=False)
            print(f"consuming queue={INPUT_QUEUE} prefix={OUTPUT_PREFIX}", flush=True)
            ch.start_consuming()
        except Exception as exc:
            print(f"consumer error: {exc}; retrying", flush=True)
            time.sleep(3)


def main():
    if TRANSPORT == "nats":
        asyncio.run(nats_main())
    else:
        rabbitmq_main()


if __name__ == "__main__":
    main()
