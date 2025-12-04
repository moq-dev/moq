import { describe, expect, it } from "vitest";
import { SubscriptionManager } from "./subscription";

describe("SubscriptionManager", () => {
	it("should initialize with no subscriptions", () => {
		const manager = new SubscriptionManager();
		expect(manager.getCount()).toBe(0);
	});

	it("should subscribe to a signal and track the subscription", () => {
		const manager = new SubscriptionManager();
		let callCount = 0;
		const unsubscribe = () => {
			callCount--;
		};
		const signal = {
			subscribe: () => unsubscribe,
		};

		manager.subscribe(signal, () => {
			callCount++;
		});

		expect(manager.getCount()).toBe(1);
	});

	it("should handle undefined signals gracefully", () => {
		const manager = new SubscriptionManager();
		manager.subscribe(undefined, () => {});
		expect(manager.getCount()).toBe(0);
	});

	it("should unsubscribe all subscriptions", () => {
		const manager = new SubscriptionManager();
		let count = 0;
		const signal = {
			subscribe: () => {
				return () => {
					count++;
				};
			},
		};

		manager.subscribe(signal, () => {});
		manager.subscribe(signal, () => {});
		manager.subscribe(signal, () => {});

		expect(manager.getCount()).toBe(3);

		manager.unsubscribeAll();

		expect(manager.getCount()).toBe(0);
		expect(count).toBe(3);
	});

	it("should execute callbacks when unsubscribing", () => {
		const manager = new SubscriptionManager();
		const callbacks: (() => void)[] = [];

		for (let i = 0; i < 3; i++) {
			const signal = {
				subscribe: () => {
					return () => {
						callbacks.push(() => {});
					};
				},
			};
			manager.subscribe(signal, () => {});
		}

		manager.unsubscribeAll();
		expect(callbacks.length).toBe(3);
	});
});
