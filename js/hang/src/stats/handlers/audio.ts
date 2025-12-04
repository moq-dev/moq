import type { HandlerContext } from "../types";
import { BaseHandler } from "./base";

/**
 * Handler for audio stream metrics (channels, bitrate, codec)
 */
export class AudioHandler extends BaseHandler {
	/** Display context for updating metrics */
	private context: HandlerContext | undefined;
	/** Polling interval ID */
	private updateInterval: number | undefined;
	/** Bound callback for display updates */
	private updateDisplay = () => this.updateDisplayData();

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

		this.updateInterval = window.setInterval(this.updateDisplay, 250);

		this.updateDisplayData();
	}

	/**
	 * Calculate and display current audio metrics
	 */
	private updateDisplayData(): void {
		if (!this.context || !this.props.audio) {
			return;
		}

		const active = this.peekSignal<string>(this.props.audio.active);
		const config = this.peekSignal<{ sampleRate?: number; numberOfChannels?: number; bitrate?: number; codec?: string }>(this.props.audio.config);
		const bitrate = this.peekSignal<number>(this.props.audio.bitrate);

		if (!active || !config) {
			this.context.setDisplayData("N/A");
			return;
		}

		const parts: string[] = [];

		if (config.sampleRate) {
			const khz = (config.sampleRate / 1000).toFixed(1);
			parts.push(`${khz}kHz`);
		}

		if (config.numberOfChannels) {
			parts.push(`${config.numberOfChannels}ch`);
		}

		const displayBitrate = bitrate ?? config.bitrate ?? (config.numberOfChannels ? config.numberOfChannels * 32_000 : undefined);

		if (displayBitrate) {
			const kbps = (displayBitrate / 1000).toFixed(0);
			parts.push(`${kbps}kbps`);
		}

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
