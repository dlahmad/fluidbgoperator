FROM python:3.14-alpine3.23

WORKDIR /app
RUN pip install --no-cache-dir pika requests
COPY app.py /app/app.py

CMD ["python", "/app/app.py"]
