#!/usr/bin/env bash
set -euo pipefail

test_filter="${1:-}"

if [ -n "$test_filter" ]; then
  cargo test -p fluidbg-e2e-tests --test e2e "$test_filter" -- --ignored --test-threads=1 --nocapture
else
  cargo test -p fluidbg-e2e-tests --test e2e -- --ignored --test-threads=1 --nocapture
fi
