FROM gcr.io/distroless/cc-debian12:nonroot
COPY dist/fluidbg-operator /usr/local/bin/fluidbg-operator
USER nonroot:nonroot
ENTRYPOINT ["fluidbg-operator"]
