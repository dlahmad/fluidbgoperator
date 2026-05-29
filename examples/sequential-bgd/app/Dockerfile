FROM python:3.14-alpine

WORKDIR /app
RUN pip install --no-cache-dir nats-py pika requests
COPY app.py /app/app.py

CMD ["python", "/app/app.py"]
