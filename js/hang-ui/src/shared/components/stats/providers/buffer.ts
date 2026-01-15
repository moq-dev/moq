import type { Getter } from "@moq/signals";
import type { BufferStatus, ProviderContext, SyncStatus } from "../types";
import { BaseProvider } from "./base";

/**
 * Provider for buffer metrics (fill percentage, latency)
 */
export class BufferProvider extends BaseProvider {
	/** Display context for updating metrics */
	private context: ProviderContext | undefined;

	/**
	 * Initialize buffer provider with signal subscriptions
	 */
	setup(context: ProviderContext): void {
		this.context = context;
		const video = this.props.video;

		if (!video) {
			context.setDisplayData("N/A");
			return;
		}

		this.signals.effect((effect) => {
			const syncStatus = effect.get(video.source.syncStatus as Getter<SyncStatus | undefined>);
			const bufferStatus = effect.get(video.source.bufferStatus as Getter<BufferStatus | undefined>);
			const latency = effect.get(video.source.latency as Getter<number | undefined>);

			const isLatencyValid = latency !== null && latency !== undefined && latency > 0;
			const bufferPercentage =
				syncStatus?.state === "wait" && syncStatus?.bufferDuration !== undefined && isLatencyValid
					? Math.min(100, Math.round((syncStatus.bufferDuration / latency) * 100))
					: bufferStatus?.state === "filled"
						? 100
						: 0;

			const parts = [`${bufferPercentage}%`, isLatencyValid ? `${latency}ms` : "N/A"];

			this.context?.setDisplayData(parts.join("\n"));
		});
	}
}
