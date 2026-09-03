#!/usr/bin/sh
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)

ADMIN_PASSWORD='xxxxx' TELEMETRY_LISTEN='0.0.0.0:8787' \
  exec "$project_dir/target/release/telemetry-server"
