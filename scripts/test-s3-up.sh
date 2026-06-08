#!/usr/bin/env bash
set -euo pipefail
# Throwaway MinIO (S3-compatible) for contract tests.
# Endpoint http://localhost:9000, creds minioadmin/minioadmin, bucket "wbtest".
docker rm -f wonderblob-test-s3 >/dev/null 2>&1 || true
docker run -d --name wonderblob-test-s3 -p 9000:9000 \
  -e MINIO_ROOT_USER=minioadmin -e MINIO_ROOT_PASSWORD=minioadmin \
  minio/minio:latest server /data >/dev/null
echo "waiting for minio..."
for i in $(seq 1 30); do
  if curl -sf http://localhost:9000/minio/health/live >/dev/null 2>&1; then
    echo "ready on http://localhost:9000 (minioadmin/minioadmin)"; exit 0
  fi
  sleep 1
done
echo "minio never came up" >&2; exit 1
