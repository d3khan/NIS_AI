# syntax=docker/dockerfile:1.7

# ---------- build stage ----------
FROM rust:1.97-bookworm AS builder

WORKDIR /app

ENV CARGO_INCREMENTAL=0 \
    CARGO_NET_RETRY=5 \
    CARGO_HTTP_TIMEOUT=120

# Cache the dependency build. The dummy main keeps the manifest valid while
# Cargo compiles every dependency; the real sources are copied afterwards.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
    && printf 'fn main() {}\n' > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src

COPY src ./src
COPY static ./static
COPY school_knowledge.md ./
RUN touch src/main.rs \
    && cargo build --release --locked

# ---------- runtime stage ----------
FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 app \
    && useradd --system --uid 10001 --gid app --home-dir /app --shell /usr/sbin/nologin app \
    && mkdir -p /app \
    && chown -R app:app /app

WORKDIR /app

COPY --from=builder --chown=app:app /app/target/release/school-chat /usr/local/bin/school-chat
COPY --from=builder --chown=app:app /app/static ./static
COPY --from=builder --chown=app:app /app/school_knowledge.md ./school_knowledge.md

# DATABASE_URL is supplied by the hosting platform (for example Render).
ENV PORT=3000 \
    RUST_LOG=info,sqlx=warn,tower_http=info,axum=info \
    LOG_FORMAT=json

EXPOSE 3000
STOPSIGNAL SIGTERM

HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD curl --fail --silent --show-error "http://127.0.0.1:${PORT}/health" || exit 1

USER app
CMD ["/usr/local/bin/school-chat"]
