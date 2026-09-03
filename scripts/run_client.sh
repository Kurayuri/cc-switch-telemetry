#!/usr/bin/sh
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)

CC_SWITCH_DB="$HOME/.cc-switch/cc-switch.db" TELEMETRY_SERVER_URL='https://token.kurayuri.cn' TELEMETRY_TOKEN=xxxxx \
  exec "$project_dir/target/release/telemetry-client"
