#!/usr/bin/env bash
set -euo pipefail
# Throwaway Azurite (Azure Storage emulator) blob service on port 10000.
# Well-known dev account: devstoreaccount1 (key is the public Azurite default).
docker rm -f wonderblob-test-azblob >/dev/null 2>&1 || true
docker run -d --name wonderblob-test-azblob -p 10000:10000 \
  mcr.microsoft.com/azure-storage/azurite:latest \
  azurite-blob --blobHost 0.0.0.0 --skipApiVersionCheck >/dev/null
echo "waiting for azurite..."
for i in $(seq 1 30); do
  if curl -s http://127.0.0.1:10000/devstoreaccount1 >/dev/null 2>&1; then
    echo "ready on http://127.0.0.1:10000 (devstoreaccount1)"; exit 0
  fi
  sleep 1
done
echo "azurite never came up" >&2; exit 1
