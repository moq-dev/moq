import type { HandlerContext, DisplaySize } from "../types";
import { BaseHandler } from "./base";

/**
 * Handler for video stream metrics (resolution, frame rate)
 */
export class VideoHandler extends BaseHandler {
	/** Display context for updating metrics */
	private context: HandlerContext | undefined;
	/** Polling interval ID */
	private updateInterval: number | undefined;
	/** Bound callback for display updates */
	private updateDisplay = () => this.updateDisplayData();

	/**
	 * Initialize video handler with polling interval
	 */
	setup(context: HandlerContext): void {
		this.context = context;
		const video = this.props.video;

		if (!video) {
			context.setDisplayData("N/A");
			return;
		}

		this.updateInterval = window.setInterval(this.updateDisplay, 250);
		this.updateDisplayData();
	}

	/**
	 * Calculate and display current video metrics
	 */
	private updateDisplayData(): void {
		if (!this.context || !this.props.video) {
			return;
		}

		const display = this.peekSignal<DisplaySize>(this.props.video?.display);
		const fps = this.peekSignal<number>(this.props.video?.fps);

		const parts = [
			display?.width && display?.height ? `${display.width}x${display.height}` : null,
			typeof fps === "number" ? `@${fps.toFixed(1)} fps` : "N/A",
		].filter((part): part is string => part !== null);

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
