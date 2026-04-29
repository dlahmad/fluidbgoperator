FROM python:3.14-alpine3.23

WORKDIR /app
RUN pip install --no-cache-dir flask pika
COPY sink.py /app/sink.py

CMD ["python", "/app/sink.py"]
