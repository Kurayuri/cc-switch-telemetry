#!/usr/bin/sh
CC_SWITCH_DB=~/.cc-switch/cc-switch.db TELEMETRY_SERVER_URL='https://token.kurayuri.cn' TELEMETRY_TOKEN=xxxxx cargo run --release -p telemetry-client
