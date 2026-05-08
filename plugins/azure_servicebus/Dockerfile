FROM gcr.io/distroless/static-debian13:nonroot
COPY dist/fluidbg-azure-servicebus /usr/local/bin/fluidbg-azure-servicebus
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/fluidbg-azure-servicebus"]
