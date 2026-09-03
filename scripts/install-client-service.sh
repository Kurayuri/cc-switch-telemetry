#!/usr/bin/sh
set -eu

service_name='cc-switch-telemetry-client.service'
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
artifact_dir=${ARTIFACT_DIR:-"$project_dir/artifacts"}
launcher_template="$project_dir/scripts/run_client.sh"
launcher="$artifact_dir/run_client.sh"
binary="$project_dir/target/release/telemetry-client"
artifact_unit="$artifact_dir/$service_name"
unit_dir=${SYSTEMD_USER_UNIT_DIR:-"${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"}
unit_file="$unit_dir/$service_name"
systemctl_bin=${SYSTEMCTL_BIN:-systemctl}
cargo_bin=${CARGO_BIN:-cargo}

fail() {
  echo "install-client-service: $*" >&2
  exit 1
}

command -v "$systemctl_bin" >/dev/null 2>&1 || fail "systemctl is required"
command -v "$cargo_bin" >/dev/null 2>&1 || fail "cargo is required"
[ -f "$launcher_template" ] || fail "missing launcher template: $launcher_template"

mkdir -p "$project_dir/data" "$artifact_dir" "$unit_dir"

if [ ! -e "$launcher" ]; then
  install -m 0755 "$launcher_template" "$launcher"
  echo "Generated launcher: $launcher"
fi
[ -f "$launcher" ] && [ -x "$launcher" ] || fail "launcher must be an executable file: $launcher"

echo "Building telemetry-client release binary..."
"$cargo_bin" build --locked --release -p telemetry-client
[ -x "$binary" ] || fail "release binary was not produced: $binary"

unit_tmp=$(mktemp "$artifact_dir/.${service_name}.XXXXXX")
trap 'rm -f -- "$unit_tmp"' EXIT HUP INT TERM

cat >"$unit_tmp" <<EOF
[Unit]
Description=CC Switch Telemetry Client
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
WorkingDirectory=$project_dir
ExecStart=$launcher
Restart=on-failure
RestartSec=5s
UMask=0077
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=$project_dir/data
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6

[Install]
WantedBy=default.target
EOF

chmod 0644 "$unit_tmp"
mv -f -- "$unit_tmp" "$artifact_unit"
trap - EXIT HUP INT TERM

ln -sfn -- "$artifact_unit" "$unit_file"

if grep -Fq 'xxxxx' "$launcher"; then
  "$systemctl_bin" --user daemon-reload
  fail "generated files; replace xxxxx in $launcher, then run this installer again"
fi

"$systemctl_bin" --user daemon-reload
"$systemctl_bin" --user enable "$service_name"
if ! "$systemctl_bin" --user restart "$service_name"; then
  "$systemctl_bin" --user --no-pager --full status "$service_name" || true
  fail "failed to start $service_name"
fi
if ! "$systemctl_bin" --user is-active --quiet "$service_name"; then
  "$systemctl_bin" --user --no-pager --full status "$service_name" || true
  fail "$service_name is not active"
fi

echo "Installed and started $service_name"
echo "Artifact unit: $artifact_unit"
echo "Systemd link: $unit_file -> $artifact_unit"
echo "Binary: $binary"
