import type { HandlerContext } from "../types";
import { BaseHandler } from "./base";

interface NavigatorWithConnection extends Navigator {
	connection?: NetworkInformation;
	mozConnection?: NetworkInformation;
	webkitConnection?: NetworkInformation;
}

interface NetworkInformation {
	type?: string;
	effectiveType?: "slow-2g" | "2g" | "3g" | "4g";
	downlink?: number;
	rtt?: number;
	saveData?: boolean;
	addEventListener?(type: string, listener: () => void): void;
	removeEventListener?(type: string, listener: () => void): void;
}

export class NetworkHandler extends BaseHandler {
	private context: HandlerContext | undefined;
	private networkInfo?: NetworkInformation;
	private updateInterval?: number;
	private updateDisplay = () => this.updateDisplayData();
	private onlineStatusChanged = () => this.updateDisplayData();

	setup(context: HandlerContext): void {
		this.context = context;

		const nav = navigator as NavigatorWithConnection;
		this.networkInfo = nav.connection ?? nav.mozConnection ?? nav.webkitConnection;

		if (this.networkInfo?.addEventListener) {
			this.networkInfo.addEventListener("change", this.updateDisplay);
		}

		window.addEventListener("online", this.onlineStatusChanged);
		window.addEventListener("offline", this.onlineStatusChanged);

		this.updateInterval = window.setInterval(this.updateDisplay, 100);
		this.updateDisplayData();
	}

	override cleanup(): void {
		if (this.networkInfo?.removeEventListener) {
			this.networkInfo.removeEventListener("change", this.updateDisplay);
		}
		window.removeEventListener("online", this.onlineStatusChanged);
		window.removeEventListener("offline", this.onlineStatusChanged);
		if (this.updateInterval !== undefined) {
			clearInterval(this.updateInterval);
		}
		super.cleanup();
	}

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

	private getLatency(): string | null {
		const rtt = this.networkInfo?.rtt;
		return rtt && rtt > 0 ? `${rtt}ms` : null;
	}

	private getSaveDataStatus(): string | null {
		return this.networkInfo?.saveData ? "Save-Data" : null;
	}
}