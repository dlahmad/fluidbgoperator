FROM python:3.14-alpine3.23

WORKDIR /app
RUN pip install --no-cache-dir pika
COPY producer.py /app/producer.py

CMD ["python", "/app/producer.py"]
