FROM rust:bookworm AS builder
WORKDIR /usr/src/fluidbg
COPY . .
RUN cargo build --release -p fluidbg-rabbitmq

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /usr/src/fluidbg/target/release/fluidbg-rabbitmq /usr/local/bin/fluidbg-rabbitmq
USER nonroot:nonroot
ENTRYPOINT ["fluidbg-rabbitmq"]
