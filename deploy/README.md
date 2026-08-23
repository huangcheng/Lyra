# Deploying Lyra

## Recommended: Docker Compose

Lyra refuses to start without a master key. Create `.env` at the repo root
(see `.env.example`; the file is gitignored):

```bash
cp .env.example .env
printf 'LYRA_MASTER_KEY=%s\n' "$(openssl rand -base64 32)" >> .env
```

| Variable | Required | Purpose |
|----------|----------|---------|
| `LYRA_MASTER_KEY` | **yes** (32+ bytes) | Master key for the per-user DEK hierarchy; all stored mail-account passwords and TOTP secrets are encrypted under it. Loss = unrecoverable credentials (re-add accounts). |

Then:

```bash
docker compose up --build -d
curl -s http://127.0.0.1:3000/health
```

Data persists in the `lyra-data` volume (`/data` in the container → SQLite by default).

### SQLite vs PostgreSQL

- **SQLite (default):** simplest single-box install. `DATABASE_URL=sqlite:/data/lyra.db`
- **PostgreSQL:** uncomment the `postgres` service in `docker-compose.yml` and set
  `DATABASE_URL=postgres://lyra:lyra@postgres:5432/lyra` on the `lyra` service.

The same migrations run on both backends.

### HTTPS

Terminate TLS at a reverse proxy (Caddy, Traefik, or nginx) in front of port 3000. Do not expose the app plain-HTTP on the public internet.

Sessions are bearer tokens in kv (memory or Redis). They are not cookie-signed; `SESSION_SECRET` is unused.

## Alternative: install script (Linux + systemd)

```bash
cd backend && cargo build --release && cd ..
sudo ./scripts/install.sh
```

See the unit `lyra.service` and `/etc/lyra.env`.

macOS/Windows: use Docker Compose.
