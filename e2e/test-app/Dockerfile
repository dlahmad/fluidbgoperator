FROM python:3.14-slim-trixie
RUN pip install flask pika requests
COPY app.py /app/app.py
WORKDIR /app
CMD ["python", "app.py"]
