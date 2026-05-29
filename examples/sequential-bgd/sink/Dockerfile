FROM python:3.14-alpine

WORKDIR /app
RUN pip install --no-cache-dir flask nats-py pika
COPY sink.py /app/sink.py

CMD ["python", "/app/sink.py"]
