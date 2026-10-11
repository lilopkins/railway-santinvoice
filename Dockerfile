FROM rust:1.90-bookworm AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates nodejs npm \
    && npm install --global @railway/cli \
    && railway --version \
    && npm cache clean --force \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/railway-santinvoice /usr/local/bin/railway-santinvoice

ENTRYPOINT ["/usr/local/bin/railway-santinvoice"]
