FROM gcr.io/distroless/static-debian12:nonroot
COPY dist/fluidbg-azure-servicebus /usr/local/bin/fluidbg-azure-servicebus
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/fluidbg-azure-servicebus"]
