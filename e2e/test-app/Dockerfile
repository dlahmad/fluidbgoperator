FROM python:3.14-slim
RUN pip install flask nats-py pika requests
COPY app.py /app/app.py
WORKDIR /app
CMD ["python", "app.py"]
