FROM gcr.io/distroless/static-debian12:nonroot
COPY dist/fluidbg-operator /usr/local/bin/fluidbg-operator
USER nonroot:nonroot
ENTRYPOINT ["fluidbg-operator"]
