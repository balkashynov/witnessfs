#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p .runs
docker build -t witnessfs-dev:local .
args=(--rm --network none --device /dev/fuse --cap-add SYS_ADMIN
  --mount "type=bind,src=$PWD,dst=/repo,readonly"
  --mount "type=bind,src=$PWD/.runs,dst=/results")
docker run "${args[@]}" -e WITNESS_RESULTS=/results/kernel-tests.json \
  witnessfs-dev:local python3 /repo/tests/fuse_integration.py
docker run "${args[@]}" witnessfs-dev:local \
  python3 /repo/scripts/record_demo.py --output /results/demo.json
if [[ "${1:-}" == "--benchmark" ]]; then
  docker run "${args[@]}" witnessfs-dev:local \
    python3 /repo/scripts/benchmark.py --output /results/benchmark.json
fi
