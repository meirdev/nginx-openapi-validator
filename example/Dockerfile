# Multi-stage build:
# 1. Build the Rust module
# 2. Copy into the matching nginx image

# ── Stage 1: Build ───────────────────────────────────────────────────
FROM rust:1.87 AS builder

RUN apt-get update && apt-get install -y \
    libclang-dev cmake make \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY . .

RUN cargo build --release -p ngx-oav-module

# ── Stage 2: Runtime ─────────────────────────────────────────────────
# The vendored nginx-src 1.28.4+1.28.2 builds nginx 1.28.2
# Use the matching official image
FROM nginx:1.28

COPY --from=builder /build/target/release/libngx_oav_module.so /etc/nginx/modules/
COPY example/nginx.conf /etc/nginx/nginx.conf
COPY example/api-spec.json /etc/nginx/api-spec.json
