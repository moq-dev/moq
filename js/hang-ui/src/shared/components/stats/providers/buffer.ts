import type { ProviderContext } from "../types";
import { BaseProvider } from "./base";

/**
 * Provider for buffer metrics (fill percentage, latency)
 */
export class BufferProvider extends BaseProvider {
	/**
	 * Initialize buffer provider with signal subscriptions
	 */
	setup(context: ProviderContext): void {
		const video = this.props.video;

		if (!video) {
			context.setDisplayData("N/A");
			return;
		}

		context.setDisplayData("TODO");
	}
}
