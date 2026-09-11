# Lyra — OpenTelemetry Observability Spec (Phase 1: traces)

**Date:** 2026-09-11
**Status:** Implemented (backend traces)
**Parent:** observability discussion; complements the Sentry integration
  ([spec](./2026-09-08-lyra-ai-byok-spec.md) era env pattern)

---

## Scope

Phase 1 ships **backend distributed traces** over OTLP/HTTP. Opt-in via
`OTEL_EXPORTER_OTLP_ENDPOINT`; unset ⇒ no provider, no layer, zero
overhead. Any OTLP receiver works — verified against both a local sink
and Sentry's native OTLP ingestion (errors and traces in one place).

Deferred: metrics (Phase 2), OTLP log export, frontend web SDK (Sentry
already covers SPA errors).

## Design

- `src/otel.rs` owns the whole SDK surface; nothing outside sees
  opentelemetry types. Plain `tracing` spans bridge via
  `tracing_opentelemetry::layer()` — instrumentation stays tracing-only.
- Env (all standard OTel names): `OTEL_EXPORTER_OTLP_ENDPOINT`,
  `OTEL_EXPORTER_OTLP_HEADERS` (`k=v,k=v`, lenient parse — tested),
  `OTEL_TRACES_SAMPLE_RATIO` (default 1.0; single-user traffic),
  `OTEL_SERVICE_NAME` (default `lyra-backend`). Compose passes all four
  through (the Sentry-whitelist lesson applied proactively).
- Spans at the deep seams, each `#[instrument]`-async-safe (a held
  `entered()` guard makes futures `!Send` — learned the hard way):
  - `lyra.sync.account` (root of every sync: HTTP/scheduled/push-woken)
  - `lyra.sync.imap` / `lyra.sync.jmap` (protocol loops)
  - `lyra.job.run` (`job_id`, `kind` — sync/unsnooze/send/backup jobs)
- Resource: `service.name`, `service.version` = crate version.

## Sharp edges hit (kept here for the next contributor)

1. **Batch processor + runtime**: `with_batch_exporter` builds a
   runtime-less processor; the async HTTP export then panics off-thread
   ("no reactor running") and batches ship empty. Fix:
   `span_processor_with_async_runtime::BatchSpanProcessor::builder(
   exporter, runtime::Tokio)` with sdk features
   `rt-tokio,experimental_trace_batch_span_processor_with_async_runtime`.
   Tests need `#[tokio::test(flavor = "multi_thread")]` (current-thread
   starves the batch task → shutdown deadlock).
2. **RUST_LOG gates spans**: EnvFilter disables info spans by default;
   production sets `RUST_LOG=info` already — keep it.
3. **Two reqwest majors** (0.12 ours, 0.13 via otel-http): do not pass
   our client to the exporter — use the builder default.
4. **Sentry OTLP URL** carries an extra segment:
   `…/api/<projectId>/integration/otlp/v1/traces`, auth via
   `x-sentry-auth: Sentry sentry_key=<public-key>`.

## Verification

- `otel::sink_live_tests::spans_reach_the_sink` (ignored test): direct
  SDK span → local sink.
- Live server against the sink: sync trigger produced a 1972-byte
  ExportTraceServiceRequest containing `lyra.sync.account →
  lyra.sync.imap` with source refs.
- Real dump POSTed to Sentry (HTTP 200) and spans visible in the
  explorer (dataset `spans`).
