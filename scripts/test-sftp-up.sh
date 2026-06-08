#!/usr/bin/env bash
set -euo pipefail
# Throwaway OpenSSH server for contract tests. User: wb / Password: wbpass / port 2222.
docker rm -f wonderblob-test-sftp >/dev/null 2>&1 || true
docker run -d --name wonderblob-test-sftp -p 2222:2222 \
  -e USER_NAME=wb -e USER_PASSWORD=wbpass -e PASSWORD_ACCESS=true \
  lscr.io/linuxserver/openssh-server:latest >/dev/null
echo "waiting for sshd..."
for i in $(seq 1 30); do
  if docker exec wonderblob-test-sftp pgrep sshd >/dev/null 2>&1; then
    sleep 1; echo "ready on localhost:2222 (wb/wbpass)"; exit 0
  fi
  sleep 1
done
echo "sshd never came up" >&2; exit 1
