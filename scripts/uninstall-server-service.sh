#!/usr/bin/sh
set -eu

service_name='cc-switch-telemetry.service'
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
artifact_dir=${ARTIFACT_DIR:-"$project_dir/artifacts"}
artifact_unit="$artifact_dir/$service_name"
unit_dir=${SYSTEMD_USER_UNIT_DIR:-"${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"}
unit_file="$unit_dir/$service_name"
systemctl_bin=${SYSTEMCTL_BIN:-systemctl}

command -v "$systemctl_bin" >/dev/null 2>&1 || {
  echo "uninstall-server-service: systemctl is required" >&2
  exit 1
}

"$systemctl_bin" --user disable --now "$service_name" >/dev/null 2>&1 || true
rm -f -- "$unit_file"
rm -f -- "$artifact_unit"
"$systemctl_bin" --user daemon-reload
"$systemctl_bin" --user reset-failed "$service_name" >/dev/null 2>&1 || true

echo "Uninstalled $service_name"
echo "Removed the systemd link and artifact unit."
echo "Preserved project data, release binaries, and the artifact launcher."
