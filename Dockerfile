# Dockerfile
FROM node:20-slim as frontend-builder
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm install
COPY . .
RUN npm run build

FROM rust:1.83 as backend-builder
WORKDIR /app
COPY . .
COPY --from=frontend-builder /app/dist ./dist
ENV SQLX_OFFLINE=true
ENV RUST_ENV=production
RUN cargo build --release
RUN cargo install sqlx-cli --no-default-features --features sqlite

FROM debian:bookworm-slim
WORKDIR /app
COPY --from=backend-builder /app/target/release/sharethis .
COPY --from=backend-builder /app/dist ./dist
COPY --from=backend-builder /app/migrations ./migrations
COPY --from=backend-builder /usr/local/cargo/bin/sqlx /usr/local/bin/sqlx

RUN apt-get update && apt-get install -y \
    ca-certificates \
    sqlite3 \
    && rm -rf /var/lib/apt/lists/*

ENV DATABASE_URL=sqlite:///app/data/sharethis.db
ENV RUST_ENV=production
EXPOSE 8080

# Start script
COPY ./dist/start.sh .
RUN chmod +x start.sh
CMD ["./start.sh"]
