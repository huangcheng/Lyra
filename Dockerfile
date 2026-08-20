# Lyra backend
FROM rust:bookworm AS backend-build
WORKDIR /app
COPY backend/Cargo.toml backend/Cargo.lock ./
COPY backend/src ./src
COPY backend/migrations ./migrations
COPY backend/rustfmt.toml backend/clippy.toml ./
RUN cargo build --release

FROM node:24-bookworm AS frontend-build
WORKDIR /app
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates libssl3 curl \
  && rm -rf /var/lib/apt/lists/*
WORKDIR /lyra
COPY --from=backend-build /app/target/release/lyra_backend /usr/local/bin/lyra_backend
COPY --from=frontend-build /app/dist /lyra/frontend/dist
COPY deploy/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh \
  && mkdir -p /data
ENV LISTEN_ADDR=0.0.0.0:3000 \
    DATABASE_URL=sqlite:/data/lyra.db \
    DATA_DIR=/data \
    FRONTEND_DIR=/lyra/frontend/dist \
    RUST_LOG=info
EXPOSE 3000
VOLUME ["/data"]
ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
