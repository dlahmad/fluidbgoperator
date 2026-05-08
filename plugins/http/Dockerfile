FROM gcr.io/distroless/static-debian13:nonroot
COPY dist/fluidbg-http /usr/local/bin/fluidbg-http
USER nonroot:nonroot
ENTRYPOINT ["fluidbg-http"]
