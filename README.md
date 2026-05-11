# Ethera Sidecar

[![Rust](https://img.shields.io/badge/rust-1.91-orange.svg)](./rust-toolchain.toml)
[![License: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0-blue.svg)](./COPYING)
[![CI](https://img.shields.io/badge/ci-cargo-green.svg)](./justfile)

**Ethera Sidecar** is a cross-chain coordination layer for rollups, built by SSV Labs. It
implements the sequencer-side logic of the Ethera protocol: the Synchronous Composability Protocol
(SCP), the Superblock Construction Protocol (SBCP), and the integration surface for the settlement
pipeline.

One sidecar runs next to each participating rollup's block builder. Sidecars communicate with each
other over HTTP, with the local builder over JSON-RPC, and with the Shared Publisher (SP) over QUIC.

---

## Overview

Ethera Sidecar owns the cross-chain transaction (XT) lifecycle on the sequencer side:

1. An XT is submitted to a sidecar (directly by a user, by a peer, or by the SP).
2. The sidecar simulates its chain-local slice of the XT against live state, tracing mailbox reads
   and writes.
3. Outbound mailbox messages are dispatched to the destination sidecar; inbound messages are
   buffered and replayed as `mailbox.putInbox` auxiliary transactions.
4. Once simulation terminates, the sidecar votes (commit / abort). Votes are aggregated either by
   the SP (in coordinated mode) or peer-to-peer (in standalone mode).
5. On a commit decision, the sidecar instructs the local builder to include the XT's transactions;
   on abort, it releases any reservation.
6. The builder confirms final inclusion back to the sidecar, closing the instance.

The sidecar enforces SBCP sequentiality (no overlapping instances on a chain), SBCP period and
superblock bookkeeping, and rollback on settlement failure.

## Status

This is an active implementation of the Ethera protocol. It currently covers:

- **SCP**: `StartInstance`, simulation with mailbox overlays, vote exchange, `Decided`, CIRC timer.
- **SBCP**: `StartPeriod`, `Rollback`, instance sequentiality, period/superblock tracking.
- **Standalone mode**: peer-to-peer vote aggregation without an SP.
- **UniversalBridgeMailbox**: traces `writeMessage(Message)` / `readMessage(MessageHeader)`,
  matches dependencies on the six-field mailbox key, and carries 256-bit session IDs as 32-byte
  big-endian protobuf bytes.
- **Builder integration**: pull-based `POST /transactions` hold/deliver flow for op-rbuilder with
  deferred nonce management for concurrent `putInbox` transactions.
- **Verification hook**: optional external HTTP callout on inbound XTs before the commit vote.

---

## Building

The toolchain is pinned to Rust 1.91 via `rust-toolchain.toml`.

```sh
just build       # cargo build --workspace
just release     # cargo build --release -p sidecar
```

A multi-stage Dockerfile is provided:

```sh
docker build -t ethera-sidecar .
```

## Running

The sidecar is configured entirely via CLI flags or `SIDECAR_*` environment variables — there is
no runtime config file. A reference `configs/config.example.yaml` documents every knob.

Minimal standalone run:

```sh
SIDECAR_CHAIN_ID=901 \
SIDECAR_CHAIN_RPC=http://localhost:8545 \
SIDECAR_PEERS=902=http://sidecar-b:8080 \
just run
```

With the Shared Publisher:

```sh
SIDECAR_PUBLISHER_ENABLED=true \
SIDECAR_PUBLISHER_ADDR=publisher:8080 \
SIDECAR_CHAIN_ID=901 \
SIDECAR_CHAIN_RPC=http://localhost:8545 \
just run
```

Key configuration groups:

| Prefix                                     | Purpose                                                   |
|--------------------------------------------|-----------------------------------------------------------|
| `SIDECAR_LISTEN_ADDR`                      | HTTP listener address                                     |
| `SIDECAR_CHAIN_*`                          | Local chain: id, RPC, builder RPC, coordinator key        |
| `SIDECAR_UNIVERSAL_BRIDGE_MAILBOX_ADDRESS` | UniversalBridgeMailbox contract address                   |
| `SIDECAR_PUBLISHER_*`                      | SP QUIC endpoint and reconnection policy                  |
| `SIDECAR_PEERS`                            | Comma-delimited peer map: `CHAIN_ID=URL[,CHAIN_ID=URL]`   |
| `SIDECAR_VERIFICATION_*`                   | External verification hook for inbound XTs                |
| `SIDECAR_LOG_*`                            | Log level (`debug`/`info`/…) and format (`json`/`pretty`) |

## Testing

```sh
just test                           # cargo test --workspace
just ci                             # fmt-check + lint + test
just ci-full                        # adds cargo-deny and cargo-machete
cargo test -p compose-coordinator   # single crate
```

---

## HTTP API

| Route                    | Direction         | Purpose                                     |
|--------------------------|-------------------|---------------------------------------------|
| `POST /xt`               | user → sidecar    | Submit a cross-chain transaction            |
| `GET  /xt/:id`           | user → sidecar    | Query XT status                             |
| `POST /xt/forward`       | peer → sidecar    | Forward an XT seen by another sidecar       |
| `POST /xt/vote`          | peer → sidecar    | Exchange SCP votes                          |
| `POST /mailbox`          | peer → sidecar    | Deliver CIRC mailbox messages               |
| `POST /transactions`     | builder ↔ sidecar | Pull-based delivery of held XT transactions |
| `POST /ethera/confirm`   | builder → sidecar | Report final inclusion of XT instances      |
| `GET  /health`, `/ready` | —                 | Liveness / readiness                        |
| `GET  /metrics`          | —                 | Prometheus exposition                       |

The SP channel is a QUIC stream carrying length-prefixed protobuf `WireMessage` frames defined in
[`crates/proto`](./crates/proto).

---

## Architecture

The workspace is one binary and a set of focused library crates, all prefixed `compose-*`:

```
bin/sidecar                — entrypoint: wires up all crates and starts HTTP + QUIC
crates/
  primitives               — shared types: ChainId, XtId, PeriodId, InstanceId, ChainState, …
  primitives-traits        — integration-boundary traits and CoordinatorError
  config                   — clap + SIDECAR_* env-var configuration
  proto                    — protobuf wire types and conversions (prost, rollup_v2)
  coordinator/
    coordinator            — XT state machine: submission → simulation → vote → decision → delivery
    server                 — axum HTTP API (routes above)
  net/
    transport              — QUIC (quinn + rustls + rcgen), TLS, length-prefixed framing
    publisher              — SP client adapter over QUIC
    peer                   — HTTP client for sidecar-to-sidecar coordination
  mailbox                  — ABI helpers, dependency matching, state overrides, in-memory queue
  simulation               — RPC-backed simulation (debug_traceCall with mailbox overlays)
  metrics                  — Prometheus counters and histograms
  tracing                  — tracing-subscriber init (JSON or pretty)
```

### XT lifecycle

The coordinator pipeline (`crates/coordinator/coordinator/src/pipeline`) maps onto SCP directly:

1. **Submission** (`pipeline/submission.rs`) — fingerprint, deduplicate, persist as `PendingXt`.
2. **Simulation** (`pipeline/simulation.rs`) — `eth_call` / `debug_traceCall` with per-chain state
   overlays so sequential XTs observe each other's effects; traces mailbox reads/writes.
3. **Voting** (`handlers/peer_vote.rs`) — exchange votes via `POST /xt/vote` (standalone) or emit
   `Vote` to SP.
4. **Decision** (`handlers/decision.rs`) — commit/abort propagates to the builder via the pull
   protocol; aborts release reservations and roll back nonces.
5. **Delivery** (`pipeline/delivery.rs`) — holds local + `putInbox` transactions until the builder
   asks for them; inclusion is confirmed via `POST /ethera/confirm`.

### SBCP integration

- `handlers/start_period.rs` tracks the current period and superblock number, clears per-period
  state, resynchronizes the `putInbox` nonce view without reusing locally reserved nonces, and
  rejects stale instances.
- `handlers/start_instance.rs` enforces sequentiality (a chain cannot be in two instances
  concurrently) and drives the SCP state machine.
- `handlers/rollback.rs` resets local state to the last finalized superblock and re-arms the
  nonce manager.

---

## Development

```sh
just fmt          # cargo fmt --all
just fmt-check    # check formatting without applying
just lint         # cargo clippy --workspace --all-targets -- -D warnings
just doc          # cargo doc --workspace --no-deps --open
just proto        # regenerate protobuf bindings
just install-hooks  # installs pre-commit hooks (requires: pip install pre-commit)
just install-tools  # installs cargo-deny and cargo-machete
```

Code-style rules enforced by CI:

- `rustfmt.toml`: `max_width = 100`, `imports_granularity = "Crate"`, `group_imports = "StdExternalCrate"`.
- Workspace lints deny `rust_2018_idioms` and `unused_must_use`, and enable a broad clippy style set
  (see `Cargo.toml`).
- `openssl` is banned; use `rustls` / `ring`.

## License

Distributed under the GNU General Public License v3.0. See [`COPYING`](./COPYING) for the full text.
