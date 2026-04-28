FROM python:3.12-alpine

WORKDIR /app
RUN pip install --no-cache-dir flask
COPY verifier.py /app/verifier.py

CMD ["python", "/app/verifier.py"]
