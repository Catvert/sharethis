#!/bin/bash
set -e

# Load .env if exists
[ -f .env ] && export $(grep -v '^#' .env | xargs)

# Configuration
DOCKER_USER="${DOCKER_USER:?DOCKER_USER is required in .env or environment}"
IMAGE_NAME="sharethis"
CONTAINER_NAME="sharethis"
PORT="${PORT:-3000}"

echo "📥 Pulling latest image..."
docker pull $DOCKER_USER/$IMAGE_NAME:latest

echo "🛑 Stopping old container..."
docker stop $CONTAINER_NAME 2>/dev/null || true
docker rm $CONTAINER_NAME 2>/dev/null || true

echo "🚀 Starting new container..."
docker run -d \
  --name $CONTAINER_NAME \
  --restart unless-stopped \
  -p $PORT:3000 \
  -e PORT=3000 \
  -e MAX_FILE_SIZE=10485760 \
  -e P2P_THRESHOLD=5242880 \
  -e DATABASE_PATH=/app/data/sharethis.db \
  -v sharethis-data:/app/data \
  $DOCKER_USER/$IMAGE_NAME:latest

echo "✅ Deployed! Running on port $PORT"
echo ""
echo "📋 Logs: docker logs -f $CONTAINER_NAME"
