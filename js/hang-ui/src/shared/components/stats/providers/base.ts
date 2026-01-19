import { Effect } from "@moq/signals";
import type { ProviderContext, ProviderProps } from "../types";

/**
 * Base class for metric providers providing common utilities
 */
export abstract class BaseProvider {
	/** Manages subscriptions lifecycle */
	protected signals = new Effect();
	/** Stream sources provided to provider */
	protected props: ProviderProps;

	/**
	 * Initialize provider with stream sources
	 * @param props - Audio and video stream sources
	 */
	constructor(props: ProviderProps) {
		this.props = props;
	}

	/**
	 * Initialize provider with display context
	 * @param context - Provider context for updating display
	 */
	abstract setup(context: ProviderContext): void;

	/**
	 * Clean up subscriptions
	 */
	cleanup(): void {
		this.signals.close();
	}
}
