FROM python:3.14-alpine

WORKDIR /app
RUN pip install --no-cache-dir nats-py pika
COPY producer.py /app/producer.py

CMD ["python", "/app/producer.py"]
