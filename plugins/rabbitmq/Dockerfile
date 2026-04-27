FROM gcr.io/distroless/static-debian12:nonroot
COPY dist/fluidbg-rabbitmq /usr/local/bin/fluidbg-rabbitmq
USER nonroot:nonroot
ENTRYPOINT ["fluidbg-rabbitmq"]
