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
			this.context?.setDisplayData("TODO");
		});
	}
}
