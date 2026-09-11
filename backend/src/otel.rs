//! OpenTelemetry tracing (Phase 1 of observability adoption — see
//! `docs/specs/2026-09-11-lyra-otel-observability-spec.md`).
//!
//! Opt-in via `OTEL_EXPORTER_OTLP_ENDPOINT`: any OTLP receiver works
//! (Sentry's `/otlp/`, an OpenTelemetry Collector, Jaeger…). Unset ⇒ no
//! provider, no layer, zero overhead. Existing `tracing` spans at the
//! module seams become OTel spans through the
//! `tracing_opentelemetry::layer()` bridge, so instrumentation is plain
//! `tracing::info_span!` — no SDK types leak past this module.

use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::trace::SdkTracerProvider;

use crate::config::Config;

/// Parse `OTEL_EXPORTER_OTLP_HEADERS` (`k=v,k=v`) into pairs; blank values
/// and malformed segments are dropped (a bad header must not kill tracing).
pub(crate) fn parse_otlp_headers(raw: &str) -> Vec<(String, String)> {
    raw.split(',')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            let (k, v) = (k.trim(), v.trim());
            (!k.is_empty() && !v.is_empty()).then(|| (k.to_string(), v.to_string()))
        })
        .collect()
}

/// Initialize the OTLP tracer provider. `None` when opt-out. The returned
/// guard must live for the whole process (flushes on drop); the
/// `tracing_opentelemetry` layer is only composed in when Some.
pub fn init_otel(config: &Config) -> Option<SdkTracerProvider> {
    let endpoint = config.otel_endpoint.as_deref()?;

    let resource = opentelemetry_sdk::Resource::builder()
        .with_service_name(config.otel_service_name.clone())
        .with_attribute(opentelemetry::KeyValue::new(
            "service.version",
            env!("CARGO_PKG_VERSION"),
        ))
        .build();

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .with_timeout(std::time::Duration::from_secs(10))
        .with_headers(
            parse_otlp_headers(config.otel_headers.as_deref().unwrap_or(""))
                .into_iter()
                .collect(),
        )
        .build()
        .map_err(|e| {
            tracing::warn!("OTLP exporter build failed: {e}");
            e
        })
        .ok()?;

    // The batch processor must own the Tokio runtime handle — the plain
    // `with_batch_exporter` builds a runtime-less processor whose async
    // HTTP export then panics off-thread ("no reactor running").
    let batch =
        opentelemetry_sdk::trace::span_processor_with_async_runtime::BatchSpanProcessor::builder(
            exporter,
            opentelemetry_sdk::runtime::Tokio,
        )
        .build();
    let provider = SdkTracerProvider::builder()
        .with_resource(resource)
        .with_span_processor(batch)
        .with_sampler(opentelemetry_sdk::trace::Sampler::ParentBased(Box::new(
            opentelemetry_sdk::trace::Sampler::TraceIdRatioBased(config.otel_sample_ratio),
        )))
        .build();

    tracing::info!(
        endpoint = endpoint,
        sample_ratio = config.otel_sample_ratio,
        service = %config.otel_service_name,
        "OpenTelemetry tracing enabled (OTLP)"
    );
    Some(provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_parse_leniently() {
        let h = parse_otlp_headers("x-sentry-auth=Sentry public_key=k, b=2 ,, =x, a=");
        assert_eq!(
            h,
            vec![
                (
                    "x-sentry-auth".to_string(),
                    "Sentry public_key=k".to_string()
                ),
                ("b".to_string(), "2".to_string()),
            ]
        );
    }
}

#[cfg(test)]
mod sink_live_tests {
    //! Manual bridge verification against a local OTLP sink
    //! (`python3 /tmp/otlp-sink.py`): proves spans flow tracing → OTel →
    //! OTLP. `OTLP_SINK=1 cargo test otel_sink -- --ignored`.

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs a local OTLP sink on :4318"]
    async fn spans_reach_the_sink() {
        use opentelemetry::trace::{TraceContextExt as _, Tracer as _, TracerProvider as _};
        use opentelemetry_otlp::WithExportConfig;

        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_endpoint("http://127.0.0.1:4318")
            .with_timeout(std::time::Duration::from_secs(3))
            .build()
            .unwrap();
        let batch =
            opentelemetry_sdk::trace::span_processor_with_async_runtime::BatchSpanProcessor::builder(
                exporter,
                opentelemetry_sdk::runtime::Tokio,
            )
            .build();
        let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
            .with_span_processor(batch)
            .build();
        let tracer = provider.tracer("sink_test");

        tracer.in_span("sink_test_root", |cx| {
            let span = cx.span();
            span.set_attribute(opentelemetry::KeyValue::new("test", "otel-bridge"));
            std::thread::sleep(std::time::Duration::from_millis(50));
        });

        provider.shutdown().unwrap();
    }
}
