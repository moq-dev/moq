import type { HandlerContext } from "../types";
import { BaseHandler } from "./base";

export class VideoHandler extends BaseHandler {
	private context: HandlerContext | undefined;
	private updateInterval: number | undefined;
	private updateDisplay = () => this.updateDisplayData();

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

	private updateDisplayData(): void {
		if (!this.context || !this.props.video) {
			return;
		}

		const display = this.peekSignal<{ width: number; height: number }>(
			this.props.video?.display
		);
		const fps = this.peekSignal<number>(this.props.video?.fps);

		const parts = [
			display?.width && display?.height ? `${display.width}x${display.height}` : null,
			fps ? `@${fps.toFixed(1)} fps` : "N/A",
		].filter((part): part is string => part !== null);

		this.context.setDisplayData(parts.join("\n"));
	}

	override cleanup(): void {
		if (this.updateInterval !== undefined) {
			clearInterval(this.updateInterval);
		}
		super.cleanup();
	}
}
