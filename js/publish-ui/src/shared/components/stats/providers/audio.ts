import type { ProviderContext } from "../types";
import { BaseProvider } from "./base";

/**
 * Provider for audio stream metrics (not applicable for publish)
 */
export class AudioProvider extends BaseProvider {
	/**
	 * Initialize audio provider - N/A for publish mode
	 */
	setup(context: ProviderContext): void {
		context.setDisplayData("N/A");
	}
}
