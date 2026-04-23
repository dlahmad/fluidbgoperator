FROM rust:bookworm AS builder
WORKDIR /usr/src/fluidbg
COPY . .
RUN cargo build --release --bin fluidbg-operator

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /usr/src/fluidbg/target/release/fluidbg-operator /usr/local/bin/fluidbg-operator
USER nonroot:nonroot
ENTRYPOINT ["fluidbg-operator"]
