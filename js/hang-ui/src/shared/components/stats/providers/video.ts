import type { ProviderContext } from "../types";
import { BaseProvider } from "./base";

/**
 * Provider for video stream metrics (resolution, frame rate, bitrate)
 */
export class VideoProvider extends BaseProvider {
	/** Polling interval in milliseconds */
	private static readonly POLLING_INTERVAL_MS = 250;
	/** Display context for updating metrics */
	private context: ProviderContext | undefined;
	/** Polling interval ID */
	private updateInterval: number | undefined;
	/** Bound callback for display updates */
	/** Previous frame count for FPS calculation */
	private previousFrameCount = 0;
	/** Previous timestamp for FPS calculation */
	private previousTimestamp = 0;
	/** Previous bytes received for bitrate calculation */
	private previousBytesReceived = 0;
	/** Previous timestamp for accurate elapsed time calculation in bitrate */
	private previousWhen = 0;

	/**
	 * Initialize video provider with polling interval
	 */
	setup(context: ProviderContext): void {
		this.context = context;
		const video = this.props.video;

		if (!video) {
			context.setDisplayData("N/A");
			return;
		}

		this.updateInterval = window.setInterval(this.updateDisplayData.bind(this), VideoProvider.POLLING_INTERVAL_MS);
		this.previousWhen = performance.now();
		this.updateDisplayData();
	}

	/**
	 * Calculate and display current video metrics
	 */
	private updateDisplayData(): void {
		if (!this.context || !this.props.video) {
			return;
		}

		const display = this.props.video.source.display.peek();
		const stats = this.props.video.source.stats.peek();
		const now = performance.now();

		// Calculate FPS from frame count delta and timestamp delta
		let fps: number | undefined;
		if (stats && this.previousTimestamp > 0) {
			const frameCountDelta = stats.frameCount - this.previousFrameCount;
			const timestampDeltaUs = stats.timestamp - this.previousTimestamp;

			if (timestampDeltaUs > 0 && frameCountDelta > 0) {
				const elapsedSeconds = timestampDeltaUs / 1_000_000;
				fps = frameCountDelta / elapsedSeconds;
			}
		}

		let bitrate: string | undefined;
		if (stats && this.previousBytesReceived > 0) {
			const bytesDelta = stats.bytesReceived - this.previousBytesReceived;
			// Only calculate bitrate if there's actual data change
			if (bytesDelta > 0) {
				const elapsedMs = now - this.previousWhen;
				if (elapsedMs > 0) {
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
		}

		// Always update previous values for next calculation, even on first call
		if (stats) {
			this.previousFrameCount = stats.frameCount;
			this.previousTimestamp = stats.timestamp;
			this.previousBytesReceived = stats.bytesReceived;
			this.previousWhen = now;
		}

		const { width, height } = display ?? {};

		const parts = [
			width && height ? `${width}x${height}` : "N/A",
			fps !== undefined ? `@${fps.toFixed(1)} fps` : "N/A",
			bitrate ?? "N/A",
		];

		this.context.setDisplayData(parts.join("\n"));
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
