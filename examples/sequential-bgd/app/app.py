import json
import os
import time
import asyncio

import nats
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


async def publish_nats_result(nc, payload):
    await nc.publish(OUTPUT_SUBJECT, json.dumps(payload).encode())
    await nc.flush()


async def handle_nats_message(nc, msg):
    try:
        order = json.loads(msg.data.decode())
        order_id = order.get("orderId")
        if not order_id:
            return

        status = audit_order(order)
        result = {
            "orderId": order_id,
            "sequence": order.get("sequence"),
            "type": "demo-result",
            "result": f"{OUTPUT_PREFIX}-{order_id}",
            "httpStatus": status,
        }
        await publish_nats_result(nc, result)
        print(f"processed order={order_id} result={result['result']} http={status}", flush=True)
    except Exception as exc:
        print(f"nats processing failed: {exc}", flush=True)


async def nats_main():
    while True:
        try:
            nc = await nats.connect(NATS_URL)

            async def callback(msg):
                await handle_nats_message(nc, msg)

            await nc.subscribe(INPUT_SUBJECT, queue=NATS_QUEUE_GROUP, cb=callback)
            print(
                f"consuming subject={INPUT_SUBJECT} queueGroup={NATS_QUEUE_GROUP} prefix={OUTPUT_PREFIX}",
                flush=True,
            )
            while True:
                await asyncio.sleep(3600)
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
