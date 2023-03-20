FROM rust:latest as builder
WORKDIR /usr/src/buzzwords
COPY . .
RUN cargo install --path .

FROM debian:buster-slim
COPY --from=builder /usr/local/cargo/bin/buzzwords /usr/local/bin/buzzwords
ENTRYPOINT ["/usr/local/bin/buzzwords"]
