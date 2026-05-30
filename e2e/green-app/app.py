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
AMQP_URL = os.environ.get("AMQP_URL", "amqp://fluidbg:fluidbg@rabbitmq.fluidbg-system:5672/")
NATS_URL = os.environ.get("NATS_URL", "nats://nats.fluidbg-system:4222")
NATS_MODE = os.environ.get("NATS_MODE", "jetstream")
INPUT_QUEUE = os.environ.get("INPUT_QUEUE", "orders")
OUTPUT_QUEUE = os.environ.get("OUTPUT_QUEUE", "results")
INPUT_SUBJECT = os.environ.get("INPUT_SUBJECT", INPUT_QUEUE)
OUTPUT_SUBJECT = os.environ.get("OUTPUT_SUBJECT", OUTPUT_QUEUE)
NATS_QUEUE_GROUP = os.environ.get("NATS_QUEUE_GROUP", "order-processor")
HTTP_UPSTREAM = os.environ.get("HTTP_UPSTREAM", "http://httpbin.org/post")
INSTANCE_NAME = os.environ.get("HOSTNAME", "unknown")
TEMP_QUEUE_DURABLE = os.environ.get("AMQP_TEMP_QUEUE_DURABLE", "false").lower() == "true"
TEMP_QUEUE_ARGUMENTS = json.loads(os.environ.get("AMQP_TEMP_QUEUE_ARGUMENTS_JSON", "{}"))
READY_FILE = "/tmp/fluidbg-ready"


def mark_ready():
    with open(READY_FILE, "w", encoding="utf-8") as ready_file:
        ready_file.write("ready\n")


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
        initial_delay = int(msg.get("greenInitialProcessingDelaySeconds", 0) or 0)

        if initial_delay > 0 and not method.redelivered:
            print(
                f"green-app delaying first delivery for {initial_delay}s "
                f"token={msg.get('recoveryToken', '<none>')} queue={INPUT_QUEUE}",
                flush=True,
            )
            time.sleep(initial_delay)

        http_status = call_http_upstream_if_required(msg)

        result = {
            "orderId": order_id,
            "httpStatus": http_status,
            "originalMessage": msg,
            "processedBy": "green",
            "instanceName": INSTANCE_NAME,
        }
        publish_json(OUTPUT_QUEUE, result)

        ch.basic_ack(method.delivery_tag)
    except Exception as e:
        print(f"green-app failed to process message: {e}", flush=True)
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


async def publish_nats_json(js, subject, payload):
    await ensure_nats_stream(js, subject)
    await js.publish(subject, json.dumps(payload).encode())


async def publish_core_nats_json(nc, subject, payload):
    await nc.publish(subject, json.dumps(payload).encode())
    await nc.flush()


async def handle_nats_message(js, msg):
    try:
        payload = json.loads(msg.data.decode())
        order_id = payload.get("orderId", "unknown")
        initial_delay = int(payload.get("greenInitialProcessingDelaySeconds", 0) or 0)
        if initial_delay > 0:
            print(
                f"green-app delaying NATS delivery for {initial_delay}s "
                f"token={payload.get('recoveryToken', '<none>')} subject={INPUT_SUBJECT}",
                flush=True,
            )
            await asyncio.sleep(initial_delay)
        http_status = call_http_upstream_if_required(payload)
        result = {
            "orderId": order_id,
            "httpStatus": http_status,
            "originalMessage": payload,
            "processedBy": "green",
            "instanceName": INSTANCE_NAME,
        }
        await publish_nats_json(js, OUTPUT_SUBJECT, result)
        await msg.ack()
    except Exception as exc:
        print(f"green-app failed to process NATS message: {exc}", flush=True)
        await msg.nak()


async def handle_core_nats_message(nc, msg):
    try:
        payload = json.loads(msg.data.decode())
        order_id = payload.get("orderId", "unknown")
        http_status = call_http_upstream_if_required(payload)
        result = {
            "orderId": order_id,
            "httpStatus": http_status,
            "originalMessage": payload,
            "processedBy": "green",
            "instanceName": INSTANCE_NAME,
        }
        await publish_core_nats_json(nc, OUTPUT_SUBJECT, result)
    except Exception as exc:
        print(f"green-app failed to process core NATS message: {exc}", flush=True)


async def core_nats_main():
    while True:
        try:
            nc = await nats.connect(NATS_URL)
            subscription = await nc.subscribe(INPUT_SUBJECT, queue=NATS_QUEUE_GROUP)
            await nc.flush()
            mark_ready()
            print(
                f"green-app consuming core NATS subject={INPUT_SUBJECT} queueGroup={NATS_QUEUE_GROUP}",
                flush=True,
            )
            while True:
                try:
                    msg = await subscription.next_msg(timeout=1)
                except TimeoutError:
                    continue
                await handle_core_nats_message(nc, msg)
        except Exception as exc:
            print(f"green-app core NATS error: {exc}, reconnecting...", flush=True)
            await asyncio.sleep(3)


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
            mark_ready()
            print(
                f"green-app consuming NATS subject={INPUT_SUBJECT} queueGroup={NATS_QUEUE_GROUP}",
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
            print(f"green-app NATS error: {exc}, reconnecting...", flush=True)
            await asyncio.sleep(3)


def rabbitmq_main():
    while True:
        try:
            conn, ch = get_channel()
            ch.basic_qos(prefetch_count=1)
            ch.basic_consume(INPUT_QUEUE, process_message, auto_ack=False)
            mark_ready()
            print(f"green-app consuming from {INPUT_QUEUE}", flush=True)
            ch.start_consuming()
        except Exception as e:
            print(f"green-app error: {e}, reconnecting...", flush=True)
            time.sleep(3)


def main():
    if TRANSPORT == "nats":
        if NATS_MODE == "core":
            asyncio.run(core_nats_main())
        else:
            asyncio.run(nats_main())
    else:
        rabbitmq_main()


if __name__ == "__main__":
    main()
