#!/bin/bash
set -e

# Load .env if exists
[ -f .env ] && export $(grep -v '^#' .env | xargs)

# Configuration
DOCKER_USER="${DOCKER_USER:?DOCKER_USER is required in .env or environment}"
IMAGE_NAME="sharethis"
TAG="${1:-latest}"

echo "🔨 Building image..."
docker build -t $DOCKER_USER/$IMAGE_NAME:$TAG .

echo "🏷️  Tagging as latest..."
docker tag $DOCKER_USER/$IMAGE_NAME:$TAG $DOCKER_USER/$IMAGE_NAME:latest

echo "📤 Pushing to Docker Hub..."
docker push $DOCKER_USER/$IMAGE_NAME:$TAG
docker push $DOCKER_USER/$IMAGE_NAME:latest

echo "✅ Done! Image pushed: $DOCKER_USER/$IMAGE_NAME:$TAG"
