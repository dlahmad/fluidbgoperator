#!/usr/bin/env bash
set -euo pipefail

cargo test -p fluidbg-e2e-tests --test e2e -- --ignored --test-threads=1 --nocapture
