import json
import os
import time

import pika
import requests

AMQP_URL = os.environ.get("AMQP_URL", "amqp://fluidbg:fluidbg@rabbitmq:5672/")
INPUT_QUEUE = os.environ.get("INPUT_QUEUE", "orders")
OUTPUT_QUEUE = os.environ.get("OUTPUT_QUEUE", "results")
OUTPUT_PREFIX = os.environ.get("OUTPUT_PREFIX", "v1")
HTTP_UPSTREAM = os.environ.get("HTTP_UPSTREAM", "")


def channel():
    connection = pika.BlockingConnection(pika.URLParameters(AMQP_URL))
    ch = connection.channel()
    ch.queue_declare(queue=INPUT_QUEUE, durable=False)
    ch.queue_declare(queue=OUTPUT_QUEUE, durable=False)
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


def main():
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


if __name__ == "__main__":
    main()
