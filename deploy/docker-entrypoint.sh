#!/usr/bin/env sh
set -eu
mkdir -p "${DATA_DIR:-/data}"
exec /usr/local/bin/lyra_backend
