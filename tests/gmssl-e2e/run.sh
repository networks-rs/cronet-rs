#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
IMAGE=${GMSSL_E2E_IMAGE:-cronet-rs-gmssl-e2e}

docker build --tag "$IMAGE" --file "$ROOT/tests/gmssl-e2e/Dockerfile" "$ROOT"
docker run --rm "$IMAGE"
