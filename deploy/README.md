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

Lyra does not terminate TLS itself — put a reverse proxy in front of port 3000.
**Do not expose the app plain-HTTP on the public internet**: sessions are bearer
tokens, and mail account credentials are only encrypted at rest after they
reach the server.

#### Option A — Caddy (automatic certificates)

`Caddyfile`:

```caddyfile
mail.example.com {
    reverse_proxy 127.0.0.1:3000
}
```

Caddy obtains and renews Let's Encrypt certificates automatically. Point your
domain's DNS at the box, install Caddy, done. Set
`LYRA_PUBLIC_URL=https://mail.example.com` in Lyra's `.env` so OAuth redirects
and absolute URLs match.

#### Option B — nginx + certbot

```bash
sudo certbot --nginx -d mail.example.com
```

```nginx
server {
    server_name mail.example.com;
    listen 443 ssl http2;
    ssl_certificate     /etc/letsencrypt/live/mail.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/mail.example.com/privkey.pem;

    location / {
        proxy_pass http://127.0.0.1:3000;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;

        # SSE (sync event stream at /api/v1/sync/events): disable buffering
        # and raise read timeouts, or events stall behind the proxy.
        proxy_buffering off;
        proxy_read_timeout 3600s;
    }
}
server {
    listen 80;
    server_name mail.example.com;
    return 301 https://$host$request_uri;
}
```

#### Notes

- Both examples must proxy **upgraded/streamed** responses untouched — the SSE
  endpoint is long-lived; buffering proxies will appear to "hang" sync updates.
- Keep `LYRA_PUBLIC_URL` set to the public HTTPS URL (required at boot).
- Sessions are bearer tokens in kv (memory or Redis). They are not
  cookie-signed; `SESSION_SECRET` is unused.

## Alternative: install script (Linux + systemd)

```bash
cd backend && cargo build --release && cd ..
sudo ./scripts/install.sh
```

See the unit `lyra.service` and `/etc/lyra.env`.

macOS/Windows: use Docker Compose.
