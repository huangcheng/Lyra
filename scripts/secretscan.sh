#!/usr/bin/env bash
# secretscan.sh — Run gitleaks against the full repository history.
#
# Usage:
#   ./scripts/secretscan.sh          # scan full history
#   ./scripts/secretscan.sh --staged # scan only staged changes (for CI / hooks)
#
# Requires: gitleaks >= 8.0  (brew install gitleaks)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONFIG="${REPO_ROOT}/.gitleaks.toml"

if ! command -v gitleaks &>/dev/null; then
  echo "error: gitleaks is not installed." >&2
  echo "  Install it: brew install gitleaks  |  https://github.com/gitleaks/gitleaks#installing" >&2
  exit 1
fi

if [[ "${1:-}" == "--staged" ]]; then
  echo "▶ gitleaks: scanning staged changes …"
  exec gitleaks protect --config="${CONFIG}" --verbose --redact --staged
else
  echo "▶ gitleaks: scanning full repository history …"
  exec gitleaks detect  --config="${CONFIG}" --verbose --redact --source="${REPO_ROOT}"
fi
