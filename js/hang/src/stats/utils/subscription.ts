/**
 * Manages signal subscriptions for handlers
 */
export class SubscriptionManager {
	/** Array of unsubscribe functions */
	private subscriptions: (() => void)[] = [];

	/**
	 * Subscribe to a signal and track unsubscribe function
	 * @param signal - Signal to subscribe to
	 * @param callback - Function called when signal changes
	 */
	subscribe(signal: { subscribe?: (callback: () => void) => () => void } | undefined, callback: () => void): void {
		const unsubscribe = signal?.subscribe?.(callback);
		if (unsubscribe) {
			this.subscriptions.push(unsubscribe);
		}
	}

	/**
	 * Unsubscribe from all tracked signals
	 */
	unsubscribeAll(): void {
		this.subscriptions.forEach((unsub) => unsub());
		this.subscriptions.length = 0;
	}

	/**
	 * Get count of active subscriptions
	 * @returns Number of tracked subscriptions
	 */
	getCount(): number {
		return this.subscriptions.length;
	}
}
