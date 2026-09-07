FROM rust:1.90-slim-bookworm AS build
WORKDIR /src
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev cmake perl make g++ \
 && rm -rf /var/lib/apt/lists/*
# Cache the dependency build across source edits.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs && echo '' > src/lib.rs \
 && cargo build --release --locked && rm -rf src
COPY src ./src
RUN touch src/main.rs src/lib.rs && cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 sfu
COPY --from=build /src/target/release/ferrite-sfu /usr/local/bin/ferrite-sfu
USER sfu
# Signaling over TCP, all media over one UDP port.
EXPOSE 7898/tcp 7899/udp
HEALTHCHECK --interval=10s --timeout=5s --start-period=3s --retries=3 \
    CMD ["/usr/local/bin/ferrite-sfu", "--health"]
ENTRYPOINT ["/usr/local/bin/ferrite-sfu"]
