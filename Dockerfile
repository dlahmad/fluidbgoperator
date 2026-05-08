FROM gcr.io/distroless/static-debian13:nonroot
COPY dist/fluidbg-operator /usr/local/bin/fluidbg-operator
USER nonroot:nonroot
ENTRYPOINT ["fluidbg-operator"]
