FROM python:3.12-alpine

WORKDIR /app
RUN pip install --no-cache-dir pika
COPY producer.py /app/producer.py

CMD ["python", "/app/producer.py"]
