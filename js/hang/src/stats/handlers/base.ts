import type { HandlerContext, HandlerProps, IStatsHandler } from "../types";
import { SubscriptionManager } from "../utils/subscription";

export abstract class BaseHandler implements IStatsHandler {
	protected subscriptionManager = new SubscriptionManager();
	protected props: HandlerProps;

	constructor(props: HandlerProps) {
		this.props = props;
	}

	abstract setup(context: HandlerContext): void;

	cleanup(): void {
		this.subscriptionManager.unsubscribeAll();
	}

	protected peekSignal<T>(signal: { peek: () => T | undefined } | undefined): T | undefined {
		return signal?.peek?.();
	}

	protected subscribe(
		signal: { subscribe?: (callback: () => void) => () => void } | undefined,
		callback: () => void
	): void {
		this.subscriptionManager.subscribe(signal, callback);
	}
}
