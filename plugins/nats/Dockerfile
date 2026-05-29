FROM gcr.io/distroless/static-debian13:nonroot
COPY dist/fluidbg-nats /usr/local/bin/fluidbg-nats
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/fluidbg-nats"]
