# Build stage
FROM rust:1-alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /app

# Build dependencies first (cached as long as the manifests don't change)
COPY Cargo.toml Cargo.lock ./
RUN mkdir src \
    && echo "fn main() {}" > src/main.rs \
    && touch src/lib.rs \
    && cargo build --release --locked \
    && rm -rf src

# Templates and scripts are compiled into the binary
COPY src ./src
COPY templates ./templates
COPY static ./static
RUN touch src/main.rs src/lib.rs && cargo build --release --locked

# Runtime stage
FROM alpine:3.24

RUN apk add --no-cache ca-certificates

WORKDIR /app

COPY --from=builder /app/target/release/sharethis .

# Data directory (SQLite database + uploaded files)
RUN mkdir -p /app/data/files

ENV PORT=3000
ENV MAX_FILE_SIZE=10485760
ENV P2P_THRESHOLD=5242880
ENV DATABASE_PATH=/app/data/sharethis.db
ENV STORAGE_DIR=/app/data/files

EXPOSE 3000

HEALTHCHECK --interval=30s --timeout=5s --retries=3 \
    CMD wget -q --spider http://localhost:3000/healthz || exit 1

CMD ["./sharethis"]
