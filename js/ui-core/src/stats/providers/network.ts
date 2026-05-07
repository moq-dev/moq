import type { ProviderContext } from "../types";
import { BaseProvider } from "./base";

/**
 * Provider for network metrics (bandwidth, latency) sourced from the
 * underlying MoQ connection (PROBE / QUIC stats).
 */
export class NetworkProvider extends BaseProvider {
	private static readonly POLLING_INTERVAL_MS = 250;
	private context: ProviderContext | undefined;

	setup(context: ProviderContext): void {
		this.context = context;

		if (!this.props.network) {
			context.setDisplayData("N/A");
			return;
		}

		this.signals.interval(this.updateDisplayData.bind(this), NetworkProvider.POLLING_INTERVAL_MS);
		this.updateDisplayData();
	}

	private updateDisplayData(): void {
		if (!this.context || !this.props.network) {
			return;
		}

		const parts = [
			formatBandwidth(this.props.network.recvBandwidth.peek(), "down"),
			formatBandwidth(this.props.network.sendBandwidth.peek(), "up"),
			formatRtt(this.props.network.rtt.peek()),
		].filter((part): part is string => part !== null);

		this.context.setDisplayData(parts.length > 0 ? parts.join("\n") : "N/A");
	}
}

function formatBandwidth(bitsPerSecond: number | undefined, direction: "up" | "down"): string | null {
	if (bitsPerSecond === undefined || bitsPerSecond <= 0) return null;

	const arrow = direction === "down" ? "↓" : "↑";
	if (bitsPerSecond >= 1_000_000_000) {
		return `${arrow} ${(bitsPerSecond / 1_000_000_000).toFixed(1)}Gbps`;
	}
	if (bitsPerSecond >= 1_000_000) {
		return `${arrow} ${(bitsPerSecond / 1_000_000).toFixed(1)}Mbps`;
	}
	if (bitsPerSecond >= 1_000) {
		return `${arrow} ${(bitsPerSecond / 1_000).toFixed(0)}kbps`;
	}
	return `${arrow} ${bitsPerSecond.toFixed(0)}bps`;
}

function formatRtt(rtt: number | undefined): string | null {
	if (rtt === undefined || rtt <= 0) return null;
	return `${rtt.toFixed(0)}ms`;
}
