FROM rust:1.91-alpine AS chef

RUN apk add --no-cache musl-dev && cargo install cargo-chef --locked

WORKDIR /app

FROM chef AS planner

COPY Cargo.lock Cargo.toml rust-toolchain.toml rustfmt.toml ./
COPY bin ./bin
COPY crates ./crates
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder

COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

COPY Cargo.lock Cargo.toml rust-toolchain.toml rustfmt.toml ./
COPY bin ./bin
COPY crates ./crates
RUN cargo build --locked --release -p sidecar

FROM alpine:3.21

RUN apk add --no-cache ca-certificates

COPY --from=builder /app/target/release/sidecar /usr/local/bin/sidecar
COPY configs /etc/sidecar/configs

EXPOSE 8090

ENTRYPOINT ["/usr/local/bin/sidecar"]
