FROM gcr.io/distroless/cc-debian12:nonroot
COPY dist/fluidbg-http /usr/local/bin/fluidbg-http
USER nonroot:nonroot
ENTRYPOINT ["fluidbg-http"]
