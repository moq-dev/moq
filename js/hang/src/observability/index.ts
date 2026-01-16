/**
 * Minimal OpenTelemetry metrics for MoQ client (browser).
 *
 * Kept intentionally small for reviewability:
 * - `moq_client_connections_total{transport=...}`
 * - `moq_client_startup_time_seconds{track_type=...}`
 */

import { metrics } from "@opentelemetry/api";
import type { Counter, Histogram, Meter } from "@opentelemetry/api";
import { OTLPMetricExporter } from "@opentelemetry/exporter-metrics-otlp-http";
import { MeterProvider, PeriodicExportingMetricReader } from "@opentelemetry/sdk-metrics";
import { Resource } from "@opentelemetry/resources";

let initialized = false;
let sessionId: string | undefined;

export interface ObservabilityConfig {
	/** OTLP endpoint URL (default: http://localhost:4318) */
	otlpEndpoint?: string;
	/** Service name (default: moq-client) */
	serviceName?: string;
	/** Enable observability (default: true if endpoint provided) */
	enabled?: boolean;
	/** Per-player session id (defaults to a random UUID) */
	sessionId?: string;
}

function createSessionId(): string {
	try {
		return globalThis.crypto?.randomUUID?.() ?? `sid-${Math.random().toString(16).slice(2)}-${Date.now()}`;
	} catch {
		return `sid-${Math.random().toString(16).slice(2)}-${Date.now()}`;
	}
}

export function initObservability(config: ObservabilityConfig = {}): void {
	if (initialized) return;

	const endpoint = config.otlpEndpoint || "http://localhost:4318";
	const serviceName = config.serviceName || "moq-client";
	const enabled = config.enabled ?? !!config.otlpEndpoint;
	sessionId = config.sessionId || createSessionId();

	if (!enabled) return;

	const resource = new Resource({
		"service.name": serviceName,
		"service.instance.id": sessionId,
		"moq.player.session_id": sessionId,
	});

	const exporterHeaders = { "Content-Type": "application/json" };
	const metricExporter = new OTLPMetricExporter({
		url: `${endpoint}/v1/metrics`,
		headers: exporterHeaders,
	});

	const reader = new PeriodicExportingMetricReader({
		exporter: metricExporter,
		exportIntervalMillis: 10000,
	});

	const meterProvider = new MeterProvider({
		resource,
		readers: [reader],
	});

	metrics.setGlobalMeterProvider(meterProvider);
	const meter = metrics.getMeter(serviceName);

	clientMetricsInstance = new ClientMetrics(meter);
	setupConnectionTracking();
	initialized = true;
}

function setupConnectionTracking() {
	// Dynamically import to avoid circular deps.
	import("@moq/lite")
		.then((Moq) => {
			if (Moq.Connection?.onConnectionType) {
				Moq.Connection.onConnectionType((type: "webtransport" | "websocket") => {
					getClientMetrics()?.recordConnection(type);
				});
			}
		})
		.catch(() => {});
}

export class ClientMetrics {
	private connectionCounter?: Counter;
	private startupTimeHistogram?: Histogram;

	constructor(meter?: Meter) {
		if (meter) {
			this.connectionCounter = meter.createCounter("moq_client_connections_total", {
				description: "Total client connections by transport type",
			});

			this.startupTimeHistogram = meter.createHistogram("moq_client_startup_time_seconds", {
				description: "Time to first audio/video frame in seconds",
				unit: "s",
			});
		}
	}

	recordConnection(transportType: "webtransport" | "websocket"): void {
		const attrs: Record<string, string> = { transport: transportType };
		if (sessionId) attrs["moq.player.session_id"] = sessionId;
		this.connectionCounter?.add(1, attrs);
	}

	recordStartupTime(seconds: number, attributes?: Record<string, string>): void {
		const attrs: Record<string, string> = { ...(attributes ?? {}) };
		if (sessionId) attrs["moq.player.session_id"] = sessionId;
		this.startupTimeHistogram?.record(seconds, attrs);
	}
}

let clientMetricsInstance: ClientMetrics | undefined;

export function getClientMetrics(): ClientMetrics | undefined {
	if (!clientMetricsInstance) clientMetricsInstance = new ClientMetrics();
	return clientMetricsInstance;
}

export function recordMetric(fn: (metrics: ClientMetrics) => void): void {
	const m = getClientMetrics();
	if (m) fn(m);
}

