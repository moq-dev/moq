import { Effect } from "@moq/signals";
import type { HandlerContext, HandlerProps, IStatsHandler } from "../types";

/**
 * Base class for metric handlers providing common utilities
 */
export abstract class BaseHandler implements IStatsHandler {
	/** Manages subscriptions lifecycle */
	protected signals = new Effect();
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
		this.signals.close();
	}
}
