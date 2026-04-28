FROM python:3.12-alpine

WORKDIR /app
RUN pip install --no-cache-dir flask pika
COPY sink.py /app/sink.py

CMD ["python", "/app/sink.py"]
