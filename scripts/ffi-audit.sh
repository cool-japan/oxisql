#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

# Detect forbidden database/TLS/crypto FFI crates that violate the Pure Rust
# policy.  The broad `-sys v` pattern is intentional; we then allowlist known
# host-platform artifacts that are not under our control:
#
#   core-foundation-sys  — macOS SDK binding pulled by chrono → iana-time-zone;
#                          absent on Linux (rust:slim Docker image).
#   Security-sys         — likewise macOS-only via security-framework.
#   arrow-string         — false positive: crate name contains "ring v"; Pure Rust
#                          Arrow string kernel, no FFI.
#
# If you add a new crate and see a false positive here, document it above and
# extend the allowlist grep below.

FORBIDDEN=$(cargo tree 2>/dev/null \
    | { grep -E '(-sys v|openssl|native-tls|ring v)' || true; } \
    | { grep -Ev '(core-foundation-sys|Security-sys|arrow-string)' || true; })

if [ -n "${FORBIDDEN}" ]; then
    echo "FFI LEAK DETECTED in oxisql:"
    echo "${FORBIDDEN}"
    exit 1
fi
echo "oxisql FFI audit: CLEAN"
