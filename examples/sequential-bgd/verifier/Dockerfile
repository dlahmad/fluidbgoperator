FROM python:3.14-alpine3.23

WORKDIR /app
RUN pip install --no-cache-dir flask requests
COPY verifier.py /app/verifier.py

CMD ["python", "/app/verifier.py"]
