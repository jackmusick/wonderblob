#!/usr/bin/env bash
# Run the SSH agent + key file auth integration tests end-to-end:
# fixture up, throwaway keys, authorize in container, private ssh-agent,
# cargo test, full cleanup. Never touches the developer's real agent.
set -euo pipefail
cd "$(dirname "$0")/.."

WORKDIR="$(mktemp -d /tmp/wbtest-auth.XXXXXX)"
KEY="$WORKDIR/wbtest_key"
KEY_PP="$WORKDIR/wbtest_key_pp"
AGENT_PID=""

cleanup() {
  [ -n "$AGENT_PID" ] && kill "$AGENT_PID" >/dev/null 2>&1 || true
  rm -rf "$WORKDIR"
  ./scripts/test-sftp-down.sh
}
trap cleanup EXIT

./scripts/test-sftp-up.sh

# Throwaway keypairs: one unencrypted, one passphrase-protected.
ssh-keygen -q -t ed25519 -N ''         -f "$KEY"
ssh-keygen -q -t ed25519 -N 'testpass' -f "$KEY_PP"

# Authorize both in the container. The linuxserver image runs the SSH user
# (wb) as uid/gid 911.
cat "$KEY.pub" "$KEY_PP.pub" | docker exec -i wonderblob-test-sftp sh -c '
  mkdir -p /config/.ssh &&
  cat >> /config/.ssh/authorized_keys &&
  chown -R 911:911 /config/.ssh &&
  chmod 700 /config/.ssh &&
  chmod 600 /config/.ssh/authorized_keys
'

# Private agent for the test run only.
eval "$(ssh-agent -s)" >/dev/null
AGENT_PID="$SSH_AGENT_PID"
ssh-add -q "$KEY"

WONDERBLOB_TEST_SFTP=1 \
WONDERBLOB_TEST_KEYFILE="$KEY" \
WONDERBLOB_TEST_KEYFILE_PP="$KEY_PP" \
SSH_AUTH_SOCK="$SSH_AUTH_SOCK" \
cargo test -p wonderblob-core --test sftp_agent -- --nocapture
