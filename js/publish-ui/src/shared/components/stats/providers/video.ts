import type { ProviderContext } from "../types";
import { BaseProvider } from "./base";

/**
 * Provider for video stream metrics (not applicable for publish)
 */
export class VideoProvider extends BaseProvider {
	/**
	 * Initialize video provider - N/A for publish mode
	 */
	setup(context: ProviderContext): void {
		context.setDisplayData("N/A");
	}
}
