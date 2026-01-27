# Build stage
FROM rust:1.83-alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /app

# Copy manifests
COPY Cargo.toml Cargo.lock* ./

# Create dummy main to cache dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release
RUN rm -rf src

# Copy source code and templates
COPY src ./src
COPY templates ./templates

# Build the application
RUN touch src/main.rs && cargo build --release

# Runtime stage
FROM alpine:3.19

RUN apk add --no-cache ca-certificates

WORKDIR /app

# Copy binary from builder
COPY --from=builder /app/target/release/sharethis .

# Copy templates
COPY templates ./templates

# Create data directory for SQLite
RUN mkdir -p /app/data

ENV PORT=3000
ENV MAX_FILE_SIZE=10485760
ENV P2P_THRESHOLD=5242880

EXPOSE 3000

CMD ["./sharethis"]
