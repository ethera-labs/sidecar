FROM rust:1.91-slim AS chef

WORKDIR /app
RUN cargo install cargo-chef --locked

FROM chef AS planner

COPY Cargo.lock Cargo.toml rust-toolchain.toml rustfmt.toml ./
COPY bin ./bin
COPY crates ./crates
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder

COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --locked --release --recipe-path recipe.json

COPY Cargo.lock Cargo.toml rust-toolchain.toml rustfmt.toml ./
COPY bin ./bin
COPY crates ./crates
RUN cargo build --locked --release --bin sidecar

FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/sidecar /usr/local/bin/sidecar

ENTRYPOINT ["/usr/local/bin/sidecar"]
