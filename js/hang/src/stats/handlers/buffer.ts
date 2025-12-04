import type { HandlerContext } from "../types";
import { BaseHandler } from "./base";

export class BufferHandler extends BaseHandler {
	private context: HandlerContext | undefined;
	private updateDisplay = () => this.updateDisplayData();

	setup(context: HandlerContext): void {
		this.context = context;
		const video = this.props.video;

		if (!video) {
			context.setDisplayData("N/A");
			return;
		}

		this.subscribe(video.syncStatus, this.updateDisplay);
		this.subscribe(video.bufferStatus, this.updateDisplay);
		this.subscribe(video.latency, this.updateDisplay);

		this.updateDisplayData();
	}

	private updateDisplayData(): void {
		if (!this.context || !this.props.video) {
			return;
		}

		const syncStatus = this.peekSignal<{ state: "ready" | "wait"; bufferDuration?: number }>(
			this.props.video?.syncStatus
		);
		const bufferStatus = this.peekSignal<{ state: "empty" | "filled" }>(
			this.props.video?.bufferStatus
		);
		const latency = this.peekSignal<number>(this.props.video.latency);

		const bufferPercentage =
			syncStatus?.state === "wait" && syncStatus?.bufferDuration !== undefined && latency
				? Math.min(100, Math.round((syncStatus.bufferDuration / latency) * 100))
				: bufferStatus?.state === "filled"
					? 100
					: 0;

		const parts = [
			`${bufferPercentage}%`,
			latency !== undefined ? `${latency}ms` : "N/A",
		];

		this.context.setDisplayData(parts.join("\n"));
	}
}
