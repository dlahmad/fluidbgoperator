import json
import os
import time
import asyncio

import nats
import pika
import requests

TRANSPORT = os.environ.get("TRANSPORT", "rabbitmq")
AMQP_URL = os.environ.get("AMQP_URL", "amqp://fluidbg:fluidbg@rabbitmq.fluidbg-system:5672/")
NATS_URL = os.environ.get("NATS_URL", "nats://nats.fluidbg-system:4222")
INPUT_QUEUE = os.environ.get("INPUT_QUEUE", "orders")
OUTPUT_QUEUE = os.environ.get("OUTPUT_QUEUE", "results")
INPUT_SUBJECT = os.environ.get("INPUT_SUBJECT", INPUT_QUEUE)
OUTPUT_SUBJECT = os.environ.get("OUTPUT_SUBJECT", OUTPUT_QUEUE)
NATS_QUEUE_GROUP = os.environ.get("NATS_QUEUE_GROUP", "order-processor")
HTTP_UPSTREAM = os.environ.get("HTTP_UPSTREAM", "http://httpbin.org/post")
PORT = int(os.environ.get("PORT", "8081"))
INSTANCE_NAME = os.environ.get("HOSTNAME", "unknown")
TEMP_QUEUE_DURABLE = os.environ.get("AMQP_TEMP_QUEUE_DURABLE", "false").lower() == "true"
TEMP_QUEUE_ARGUMENTS = json.loads(os.environ.get("AMQP_TEMP_QUEUE_ARGUMENTS_JSON", "{}"))


def queue_declaration(queue):
    if queue.startswith("fluidbg-"):
        return {"durable": TEMP_QUEUE_DURABLE, "arguments": TEMP_QUEUE_ARGUMENTS}
    return {"durable": True, "arguments": None}


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


async def publish_nats_json(nc, subject, payload):
    await nc.publish(subject, json.dumps(payload).encode())
    await nc.flush()


async def handle_nats_message(nc, msg):
    try:
        body = msg.data.decode()
        payload = json.loads(body)
        order_id = payload.get("orderId", "unknown")
        http_status = call_http_upstream_if_required(payload)
        result = {
            "orderId": order_id,
            "httpStatus": http_status,
            "originalMessage": payload,
            "processedBy": "blue",
            "instanceName": INSTANCE_NAME,
        }
        await publish_nats_json(nc, OUTPUT_SUBJECT, result)
    except Exception as exc:
        print(f"blue-app failed to process NATS message: {exc}", flush=True)


async def nats_main():
    while True:
        try:
            nc = await nats.connect(NATS_URL)
            async def callback(msg):
                await handle_nats_message(nc, msg)

            await nc.subscribe(INPUT_SUBJECT, queue=NATS_QUEUE_GROUP, cb=callback)
            print(
                f"blue-app consuming NATS subject={INPUT_SUBJECT} queueGroup={NATS_QUEUE_GROUP}",
                flush=True,
            )
            while True:
                await asyncio.sleep(3600)
        except Exception as exc:
            print(f"blue-app NATS error: {exc}, reconnecting...", flush=True)
            await asyncio.sleep(3)


def rabbitmq_main():
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


def main():
    if TRANSPORT == "nats":
        asyncio.run(nats_main())
    else:
        rabbitmq_main()


if __name__ == "__main__":
    main()
