import type { HandlerContext, HandlerProps, IStatsHandler } from "../types";
import { SubscriptionManager } from "../utils/subscription";

/**
 * Base class for metric handlers providing common utilities
 */
export abstract class BaseHandler implements IStatsHandler {
	/** Manages signal subscriptions */
	protected subscriptionManager = new SubscriptionManager();
	/** Stream sources provided to handler */
	protected props: HandlerProps;

	/**
	 * Initialize handler with stream sources
	 * @param props - Audio and video stream sources
	 */
	constructor(props: HandlerProps) {
		this.props = props;
	}

	/**
	 * Initialize handler with display context
	 * @param context - Handler context for updating display
	 */
	abstract setup(context: HandlerContext): void;

	/**
	 * Clean up subscriptions
	 */
	cleanup(): void {
		this.subscriptionManager.unsubscribeAll();
	}

	/**
	 * Get current value from signal
	 * @param signal - Signal to peek
	 * @returns Current signal value or undefined
	 */
	protected peekSignal<T>(signal: { peek: () => T | undefined } | undefined): T | undefined {
		return signal?.peek?.();
	}

	/**
	 * Subscribe to signal changes
	 * @param signal - Signal to subscribe to
	 * @param callback - Function called on signal change
	 */
	protected subscribe(
		signal: { subscribe?: (callback: () => void) => () => void } | undefined,
		callback: () => void
	): void {
		this.subscriptionManager.subscribe(signal, callback);
	}
}
