# syntax=docker/dockerfile:1

FROM rust:1.95-slim AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        cmake \
        perl \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Compile dependencies first so they stay cached when only the source changes.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src \
    && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src

COPY src ./src
COPY static ./static

# Rebuild only this crate, then shrink the binary.
RUN touch src/main.rs \
    && cargo build --release --locked \
    && strip target/release/axum-streaming

FROM debian:trixie-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/axum-streaming /usr/local/bin/axum-streaming

ENV HOST=0.0.0.0 \
    PORT=3977

EXPOSE 3977

USER 10001:10001

CMD ["axum-streaming"]
