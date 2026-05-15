# syntax=docker/dockerfile:1.7
#
# OpenProxy — single-binary Docker image
#
# Three-stage build:
#   1. web    — pnpm install + astro build → web/dist/
#   2. rust   — cargo build --release with embedded web/dist via rust-embed
#   3. runtime — debian:bookworm-slim + the binary + ca-certificates
#
# Final image is ~80 MB (debian-slim base + the openproxy binary, which
# already contains the dashboard via rust-embed).
#
# Build:    docker build -t openproxy .
# Run:      docker run -d --name openproxy -p 4623:4623 \
#               -v openproxy-data:/app/data openproxy

# ──────────────────────────────────────────────────────────────────────────
# Stage 1: build the dashboard
# ──────────────────────────────────────────────────────────────────────────
FROM node:20-bookworm-slim AS web
WORKDIR /web

# pnpm via corepack — version pinned to match web/package.json packageManager
RUN corepack enable && corepack prepare pnpm@10.33.2 --activate

# Copy the lockfile + manifest first so the install layer caches when the
# rest of web/ changes but dependencies don't.
COPY web/package.json web/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile

# Copy the rest and build.
COPY web/ ./
RUN pnpm run build
# → /web/dist/

# ──────────────────────────────────────────────────────────────────────────
# Stage 2: build the binary with the embedded dashboard
# ──────────────────────────────────────────────────────────────────────────
FROM rust:1-bookworm AS rust
WORKDIR /src

# Install build deps for crates that need them at compile time.
# rusqlite/bundled handles its own SQLite. reqwest/rustls handles its own TLS.
# We only need pkg-config and a working linker, both already in rust:bookworm.
# (apt update kept minimal.)
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config \
    && rm -rf /var/lib/apt/lists/*

# Copy the workspace. Cargo.lock + manifests first for layer caching, then
# the source tree, then the embedded dashboard from stage 1.
COPY Cargo.toml Cargo.lock build.rs ./
COPY src/ ./src/
# rust-embed reads web/dist/ at compile time; src/server/api/mod.rs also
# include_str!s web/package.json. Both must exist before `cargo build`.
COPY --from=web /web/dist/ ./web/dist/
COPY --from=web /web/package.json ./web/package.json

# Build with the default `embed-web` feature on. build.rs verifies
# web/dist/index.html exists before invoking rust-embed.
RUN cargo build --release --locked --bin openproxy
RUN strip /src/target/release/openproxy

# ──────────────────────────────────────────────────────────────────────────
# Stage 3: minimal runtime
# ──────────────────────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

# ca-certificates: required for outbound HTTPS to provider APIs.
# tini: clean signal handling for ctrl+c / SIGTERM in the container.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tini \
    && rm -rf /var/lib/apt/lists/*

COPY --from=rust /src/target/release/openproxy /usr/local/bin/openproxy

# Container-friendly defaults: bind 0.0.0.0 (so port-forward works) and
# keep state under /app/data which is mounted as a volume.
ENV HOSTNAME=0.0.0.0 \
    PORT=4623 \
    DATA_DIR=/app/data

WORKDIR /app
VOLUME ["/app/data"]

EXPOSE 4623

# tini reaps zombies and forwards signals; --no-open avoids any browser
# launch attempt inside the container.
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/openproxy"]
CMD ["--no-open"]
