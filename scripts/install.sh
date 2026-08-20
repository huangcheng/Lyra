#!/usr/bin/env bash
# Install Lyra backend binary + systemd unit (Linux).
# Usage: sudo ./scripts/install.sh [/usr/local]
set -euo pipefail

PREFIX="${1:-/usr/local}"
BIN_DIR="${PREFIX}/bin"
UNIT_DIR="/etc/systemd/system"
DATA_DIR="/var/lib/lyra"
ENV_FILE="/etc/lyra.env"

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "This install script currently targets Linux + systemd." >&2
  echo "On macOS/Windows, use Docker Compose: docker compose up --build -d" >&2
  exit 1
fi

if [[ ! -f target/release/lyra_backend && ! -f backend/target/release/lyra_backend ]]; then
  echo "Build the release binary first:" >&2
  echo "  cd backend && cargo build --release" >&2
  exit 1
fi

SRC="backend/target/release/lyra_backend"
[[ -f target/release/lyra_backend ]] && SRC="target/release/lyra_backend"

install -d "$BIN_DIR" "$DATA_DIR"
install -m 755 "$SRC" "$BIN_DIR/lyra_backend"

if [[ ! -f "$ENV_FILE" ]]; then
  cat >"$ENV_FILE" <<EOF
LISTEN_ADDR=0.0.0.0:3000
DATABASE_URL=sqlite:${DATA_DIR}/lyra.db
DATA_DIR=${DATA_DIR}
RUST_LOG=info
# SESSION_SECRET=$(openssl rand -hex 32)
EOF
  chmod 600 "$ENV_FILE"
fi

cat >"${UNIT_DIR}/lyra.service" <<EOF
[Unit]
Description=Lyra self-hosted mail client
After=network.target

[Service]
Type=simple
EnvironmentFile=${ENV_FILE}
ExecStart=${BIN_DIR}/lyra_backend
Restart=on-failure
WorkingDirectory=${DATA_DIR}
User=root

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable --now lyra.service
echo "Lyra installed. Env: ${ENV_FILE}. Health: curl -s http://127.0.0.1:3000/health"
echo "Put a reverse proxy (Caddy/nginx) in front for HTTPS in production."
