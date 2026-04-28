FROM python:3.12-alpine

WORKDIR /app
RUN pip install --no-cache-dir pika requests
COPY app.py /app/app.py

CMD ["python", "/app/app.py"]
