import { expect, spyOn, test } from "bun:test";
import { type Dispose, Signal, type Subscriber } from "@moq/signals";
import { Producer } from "./group.ts";
import { sendPayload } from "./payload.ts";

// A stopped sender must release its wait even when the source frame stays open forever.
// Otherwise each cancelled FETCH leaves a reader and its listeners on the shared frame.
test("stopping a payload releases its pending chunk wait", async () => {
	const producer = new Producer(0);
	producer.createFrame({ size: 2 });
	const group = producer.consume();
	const frame = await group.nextFrame();
	if (!frame) throw new Error("missing frame");
	let stop!: () => void;
	const closed = new Promise<void>((resolve) => (stop = resolve));
	let listeners = 0;
	const changed = Signal.prototype.changed;
	function tracked(this: Signal<unknown>): Promise<unknown>;
	function tracked(this: Signal<unknown>, fn: Subscriber<unknown>): Dispose;
	function tracked(this: Signal<unknown>, fn?: Subscriber<unknown>): Promise<unknown> | Dispose {
		if (!fn) return new Promise((resolve) => this.changed(resolve));
		const dispose = changed.call(this, fn);
		listeners++;
		return () => {
			listeners--;
			dispose();
		};
	}
	const watch = spyOn(Signal.prototype, "changed").mockImplementation(tracked);
	try {
		const sending = sendPayload(
			frame,
			async () => {
				throw new Error("no bytes were written");
			},
			closed,
		);
		expect(listeners).toBeGreaterThan(0);
		stop();
		await expect(sending).rejects.toThrow("stream closed mid-frame");
		expect(listeners).toBe(0);
	} finally {
		watch.mockRestore();
		producer.abort(new Error("test ended"));
		group.close();
	}
});
