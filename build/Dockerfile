# syntax=docker/dockerfile:1
FROM rust:1.91-alpine AS builder

RUN apk add --no-cache musl-dev protobuf-dev openssl-dev openssl-libs-static git openssh-client

ENV CARGO_NET_GIT_FETCH_WITH_CLI=true

WORKDIR /app
COPY . .
RUN --mount=type=ssh \
    mkdir -p -m 0700 ~/.ssh && \
    ssh-keyscan github.com >> ~/.ssh/known_hosts && \
    git config --global url."git@github.com:".insteadOf "https://github.com/" && \
    cargo build --release -p sidecar

FROM alpine:3.21

RUN apk add --no-cache ca-certificates wget

COPY --from=builder /app/target/release/sidecar /usr/local/bin/sidecar
COPY --from=builder /app/configs /etc/sidecar/configs

EXPOSE 8090

ENTRYPOINT ["/usr/local/bin/sidecar"]
