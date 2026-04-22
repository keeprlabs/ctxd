# ADR-005: Optional OpenTelemetry Integration

## Context

ctxd is infrastructure software that agents depend on for context storage and
retrieval. When running in production or debugging latency issues, operators
need distributed tracing to understand where time is spent across append, read,
search, and MCP tool calls. The tracing crate is already used throughout the
codebase for structured logging; OpenTelemetry builds on top of it.

## Decision

Add optional OpenTelemetry (OTLP) support, controlled entirely by environment
variables at startup:

- If `OTEL_EXPORTER_OTLP_ENDPOINT` is set, an OTLP/gRPC exporter layer is
  added to the tracing subscriber. Spans from `tracing::instrument` are
  exported to the configured backend (Jaeger, Grafana Tempo, Honeycomb, etc.).
- If the variable is **not** set, the subscriber uses only the plain `fmt`
  layer. No OTEL crates are initialized, no background threads are spawned,
  and no gRPC connections are opened. The overhead is zero beyond the
  compile-time dependency.

Key operations in `ctxd-store` (`append`, `read`, `search`) are annotated with
`#[tracing::instrument]` so they produce meaningful spans regardless of whether
OTEL is active. When OTEL is inactive these annotations still produce
`tracing` events consumed by the fmt layer.

The OTLP exporter is shut down cleanly via a drop guard on the tracer provider,
ensuring in-flight spans are flushed before the process exits.

## Consequences

- Users can connect ctxd to any OTLP-compatible observability backend by
  setting a single environment variable.
- No configuration files, feature flags, or CLI arguments are needed.
- When OTEL is disabled (the default), there is no runtime cost beyond what
  `tracing` already imposes.
- The `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, and
  `tracing-opentelemetry` crates are compile-time dependencies of `ctxd-cli`.
  They do not affect other crates in the workspace.
