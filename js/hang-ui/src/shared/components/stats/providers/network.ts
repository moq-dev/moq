import type { ProviderContext } from "../types";
import { BaseProvider } from "./base";

/**
 * Extended Navigator interface with connection property
 */
interface NavigatorWithConnection extends Navigator {
	/** Standard Network Information API */
	connection?: NetworkInformation;
	/** Mozilla variant */
	mozConnection?: NetworkInformation;
	/** WebKit variant */
	webkitConnection?: NetworkInformation;
}

/**
 * Network information interface from navigator.connection
 */
interface NetworkInformation {
	/** Connection type (wifi, cellular, etc) */
	type?: string;
	/** Effective connection speed category */
	effectiveType?: "slow-2g" | "2g" | "3g" | "4g";
	/** Downlink speed in Mbps */
	downlink?: number;
	/** Round-trip time in ms */
	rtt?: number;
	/** User has enabled data saver mode */
	saveData?: boolean;
	/** Listen for connection changes */
	addEventListener?(type: string, listener: () => void): void;
	/** Stop listening for connection changes */
	removeEventListener?(type: string, listener: () => void): void;
}

/**
 * Provider for network metrics (connection type, bandwidth, latency)
 */
export class NetworkProvider extends BaseProvider {
	/** Polling interval in milliseconds */
	private static readonly POLLING_INTERVAL_MS = 100;
	/** Display context for updating metrics */
	private context: ProviderContext | undefined;
	/** Network information from navigator.connection */
	private networkInfo?: NetworkInformation;
	/** Polling interval ID */
	private updateInterval?: number;
	private readonly boundUpdateDisplayData = this.updateDisplayData.bind(this);

	/**
	 * Initialize network provider with connection listeners
	 */
	setup(context: ProviderContext): void {
		this.context = context;

		const nav = navigator as NavigatorWithConnection;
		this.networkInfo = nav.connection ?? nav.mozConnection ?? nav.webkitConnection;

		if (!this.networkInfo) {
			context.setDisplayData("N/A");
			return;
		}

		this.networkInfo.addEventListener?.("change", this.boundUpdateDisplayData);

		window.addEventListener("online", this.boundUpdateDisplayData);
		window.addEventListener("offline", this.boundUpdateDisplayData);

		this.updateInterval = window.setInterval(this.boundUpdateDisplayData, NetworkProvider.POLLING_INTERVAL_MS);
		this.updateDisplayData();
	}

	/**
	 * Clean up event listeners and polling interval
	 */
	override cleanup(): void {
		if (this.networkInfo?.removeEventListener) {
			this.networkInfo.removeEventListener("change", this.boundUpdateDisplayData);
		}
		window.removeEventListener("online", this.boundUpdateDisplayData);
		window.removeEventListener("offline", this.boundUpdateDisplayData);
		if (this.updateInterval !== undefined) {
			clearInterval(this.updateInterval);
		}
		super.cleanup();
	}

	/**
	 * Calculate and display current network metrics
	 */
	private updateDisplayData(): void {
		if (!this.context) {
			return;
		}

		const parts = [
			this.getConnectionType(),
			this.getEffectiveBandwidth(),
			this.getLatency(),
			this.getSaveDataStatus(),
		].filter((part): part is string => part !== null);

		this.context.setDisplayData(parts.length > 0 ? parts.join("\n") : "N/A");
	}

	/**
	 * Get formatted connection type
	 * @returns Connection type or null if unavailable
	 */
	private getConnectionType(): string | null {
		if (!navigator.onLine) {
			return "offline";
		}

		if (!this.networkInfo) {
			return null;
		}

		const effectiveType = this.networkInfo.effectiveType;
		if (effectiveType) {
			const typeMap = {
				"slow-2g": "Slow-2G",
				"2g": "2G",
				"3g": "3G",
				"4g": "4G",
			};
			return typeMap[effectiveType];
		}

		const type = this.networkInfo.type;
		return type ? type.charAt(0).toUpperCase() + type.slice(1) : null;
	}

	/**
	 * Get formatted bandwidth in Mbps or Gbps
	 * @returns Bandwidth string or null if unavailable
	 */
	private getEffectiveBandwidth(): string | null {
		const downlink = this.networkInfo?.downlink;
		if (!downlink || downlink <= 0) return null;

		if (downlink >= 1000) {
			return `${(downlink / 1000).toFixed(1)}Gbps`;
		}
		if (downlink >= 1) {
			return `${downlink.toFixed(1)}Mbps`;
		}
		return `${(downlink * 1000).toFixed(0)}Kbps`;
	}

	/**
	 * Get formatted round-trip latency
	 * @returns Latency string or null if unavailable
	 */
	private getLatency(): string | null {
		const rtt = this.networkInfo?.rtt;
		return rtt && rtt > 0 ? `${rtt}ms` : null;
	}

	/**
	 * Get data saver mode status
	 * @returns Data saver indicator or null if disabled
	 */
	private getSaveDataStatus(): string | null {
		return this.networkInfo?.saveData ? "Save-Data" : null;
	}
}
