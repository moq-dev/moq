import type { HandlerContext } from "../types";
import { BaseHandler } from "./base";

/**
 * Handler for audio stream metrics (channels, bitrate, codec)
 */
export class AudioHandler extends BaseHandler {
	/** Polling interval in milliseconds */
	private static readonly POLLING_INTERVAL_MS = 250;
	/** Display context for updating metrics */
	private context: HandlerContext | undefined;
	/** Polling interval ID */
	private updateInterval: number | undefined;
	/** Bound callback for display updates */
	private updateDisplay = () => this.updateDisplayData();
	/** Previous bytes received for bitrate calculation */
	private previousBytesReceived = 0;
	/** Previous timestamp for accurate elapsed time calculation */
	private previousWhen = 0;

	/**
	 * Initialize audio handler with polling interval
	 */
	setup(context: HandlerContext): void {
		this.context = context;
		const audio = this.props.audio;

		if (!audio) {
			context.setDisplayData("N/A");
			return;
		}

		this.updateInterval = window.setInterval(this.updateDisplay, AudioHandler.POLLING_INTERVAL_MS);

		this.previousWhen = performance.now();
		this.updateDisplayData();
	}

	/**
	 * Calculate and display current audio metrics
	 */
	private updateDisplayData(): void {
		if (!this.context || !this.props.audio) {
			return;
		}

		const active = this.props.audio.source.active.peek();

		const config = this.props.audio.source.config.peek();

		const stats = this.props.audio.source.stats.peek();

		if (!active || !config) {
			this.context.setDisplayData("N/A");
			return;
		}

		const now = performance.now();
		let bitrate: string | undefined;
		if (stats && this.previousBytesReceived > 0) {
			const bytesDelta = stats.bytesReceived - this.previousBytesReceived;
			// Only calculate bitrate if there's actual data change
			if (bytesDelta > 0) {
				const elapsedMs = now - this.previousWhen;
				const bitsPerSecond = bytesDelta * 8 * (1000 / elapsedMs);

				if (bitsPerSecond >= 1_000_000) {
					bitrate = `${(bitsPerSecond / 1_000_000).toFixed(1)}Mbps`;
				} else if (bitsPerSecond >= 1_000) {
					bitrate = `${(bitsPerSecond / 1_000).toFixed(0)}kbps`;
				} else {
					bitrate = `${bitsPerSecond.toFixed(0)}bps`;
				}
			}
		}

		// Always update previous values for next calculation, even on first call
		if (stats) {
			this.previousBytesReceived = stats.bytesReceived;
			this.previousWhen = now;
		}

		const parts: string[] = [];

		if (config.sampleRate) {
			const khz = (config.sampleRate / 1000).toFixed(1);
			parts.push(`${khz}kHz`);
		}

		if (config.numberOfChannels) {
			parts.push(`${config.numberOfChannels}ch`);
		}

		parts.push(bitrate ?? "N/A");

		if (config.codec) {
			parts.push(config.codec);
		}

		this.context.setDisplayData(parts.length > 0 ? parts.join("\n") : "N/A");
	}

	/**
	 * Clean up polling interval
	 */
	override cleanup(): void {
		if (this.updateInterval !== undefined) {
			clearInterval(this.updateInterval);
		}
		super.cleanup();
	}
}
