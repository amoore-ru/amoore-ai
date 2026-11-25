# amoore-ai-rust: сборка в rust:1.80-slim, запуск в debian:bookworm-slim.
# Контекст сборки на Render — папка amoore-ai-rust/ (Root Directory), Dockerfile лежит в её корне.

# ---------- сборка ----------
FROM rust:1.80-slim AS builder
WORKDIR /app

# 1) слой с зависимостями: пересобирается только при изменении Cargo.toml / Cargo.lock
COPY Cargo.toml Cargo.lock* ./
RUN mkdir src && echo "fn main() {}" > src/main.rs \
    && cargo build --release \
    && rm -rf src target/release/deps/amoore_ai_rust* target/release/amoore-ai-rust

# 2) сам код
COPY src ./src
RUN cargo build --release

# ---------- запуск ----------
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --no-create-home amoore
COPY --from=builder /app/target/release/amoore-ai-rust /usr/local/bin/amoore-ai-rust
USER amoore
ENV RUST_LOG=info
# Render подставляет PORT сам; бот слушает 0.0.0.0:$PORT (по умолчанию 10000)
EXPOSE 10000
CMD ["amoore-ai-rust"]
