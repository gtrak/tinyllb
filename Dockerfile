# ---------- Builder ----------
FROM rust:slim AS builder

RUN apt-get update && \
    apt-get install -y --no-install-recommends libssl-dev pkg-config gcc && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /src

# Cache layer: copy manifest + benches so cargo dependency resolution is cached.
COPY Cargo.toml Cargo.lock ./
COPY benches ./benches
RUN mkdir src && echo "fn main() {}" > src/main.rs && \
    cargo build --release && \
    rm -rf src

# Full build.
COPY src ./src
COPY Cargo.toml Cargo.lock ./
RUN cargo build --release

# ---------- Runtime ----------
FROM debian:bookworm-slim AS runtime

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates curl && \
    rm -rf /var/lib/apt/lists/* && \
    groupadd -f --system proxy && \
    useradd --system --gid proxy --no-create-home --home-dir /etc/llm-qdisc proxy 2>/dev/null || true

COPY --from=builder /src/target/release/llm-qdisc-proxy /usr/local/bin/
COPY config.example.yaml /etc/llm-qdisc/config.example.yaml

USER proxy
EXPOSE 8080
ENV CONFIG_PATH=/etc/llm-qdisc/config.yaml

ENTRYPOINT ["llm-qdisc-proxy"]
