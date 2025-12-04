export class SubscriptionManager {
	private subscriptions: (() => void)[] = [];

	subscribe(signal: { subscribe?: (callback: () => void) => () => void } | undefined, callback: () => void): void {
		const unsubscribe = signal?.subscribe?.(callback);
		if (unsubscribe) {
			this.subscriptions.push(unsubscribe);
		}
	}

	unsubscribeAll(): void {
		this.subscriptions.forEach((unsub) => unsub());
		this.subscriptions.length = 0;
	}

	getCount(): number {
		return this.subscriptions.length;
	}
}
