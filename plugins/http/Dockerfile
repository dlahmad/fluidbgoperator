FROM gcr.io/distroless/static-debian12:nonroot
COPY dist/fluidbg-http /usr/local/bin/fluidbg-http
USER nonroot:nonroot
ENTRYPOINT ["fluidbg-http"]
