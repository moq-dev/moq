import { describe, expect, test } from "bun:test";
import { yieldNow } from "@moq/clock";
import { SendScheduler, type SendSink } from "./scheduler.ts";

/** A sink that starts backpressured; `release()` admits exactly one write. */
class GatedSink implements SendSink {
	written: number[] = [];
	#gate: Promise<void>;
	#open!: () => void;
	#admits = 0;
	#onWrite?: () => void;

	constructor() {
		this.#gate = new Promise((resolve) => {
			this.#open = resolve;
		});
	}

	/** Record each written frame's first byte so tests can assert ordering. */
	onWrite(fn: () => void) {
		this.#onWrite = fn;
	}

	/** When set, the next `write()` rejects with this error. */
	failNext?: Error;

	async ready(): Promise<void> {
		while (this.#admits <= 0) {
			await this.#gate;
			this.#gate = new Promise((resolve) => {
				this.#open = resolve;
			});
		}
	}

	async write(bytes: Uint8Array): Promise<void> {
		this.#admits -= 1;
		if (this.failNext) {
			const err = this.failNext;
			this.failNext = undefined;
			throw err;
		}
		this.written.push(bytes[0]);
		this.#onWrite?.();
	}

	/** Allow `n` more writes through. */
	release(n = 1): void {
		this.#admits += n;
		this.#open();
	}

	/** Synchronous backpressure probe: room iff there's an unused admit. */
	wantsMore(): boolean {
		return this.#admits > 0;
	}
}

/** Yield to the event loop `n` times to let queued work run. */
async function tick(n = 1): Promise<void> {
	for (let i = 0; i < n; i++) {
		await yieldNow();
	}
}

/** Wait until `fn()` is true, throwing if it never becomes true within budget —
 *  so a scheduler regression fails the test instead of silently proceeding. */
async function until(fn: () => boolean, tries = 200): Promise<void> {
	for (let i = 0; i < tries && !fn(); i++) {
		await yieldNow();
	}
	if (!fn()) throw new Error("until: condition not met within timeout");
}

const frame = (tag: number) => new Uint8Array([tag]);

describe("SendScheduler", () => {
	test("control frames preempt stream data", async () => {
		const sink = new GatedSink();
		const sched = new SendScheduler(sink);

		sched.setSendOrder(1n, 0);
		void sched.enqueueStream(1n, frame(10));
		void sched.enqueueStream(2n, frame(20));
		sched.enqueueControl(frame(99));

		await tick(5); // let everything queue
		sink.release(3);
		await until(() => sink.written.length === 3);

		expect(sink.written[0]).toBe(99); // control first
	});

	test("higher sendOrder is sent first", async () => {
		const sink = new GatedSink();
		const sched = new SendScheduler(sink);

		sched.setSendOrder(1n, 5);
		sched.setSendOrder(2n, 1);
		sched.setSendOrder(3n, 9);
		void sched.enqueueStream(1n, frame(5));
		void sched.enqueueStream(2n, frame(1));
		void sched.enqueueStream(3n, frame(9));

		await tick(5);
		sink.release(3);
		await until(() => sink.written.length === 3);

		expect(sink.written).toEqual([9, 5, 1]);
	});

	test("round-robin among equal sendOrder", async () => {
		const sink = new GatedSink();
		const sched = new SendScheduler(sink);
		sched.setSendOrder(1n, 0);
		sched.setSendOrder(2n, 0);

		// Each stream re-arms after its frame is written, like a real writer task.
		let aLeft = 3;
		let bLeft = 3;
		const armA = () => {
			if (aLeft-- > 0) void sched.enqueueStream(1n, frame(0xa)).then(armA);
		};
		const armB = () => {
			if (bLeft-- > 0) void sched.enqueueStream(2n, frame(0xb)).then(armB);
		};
		armA();
		armB();

		await tick(5);
		sink.release(6);
		await until(() => sink.written.length === 6);

		// Strict alternation A,B,A,B,A,B (0xa, 0xb).
		expect(sink.written).toEqual([0xa, 0xb, 0xa, 0xb, 0xa, 0xb]);
	});

	test("promoting a stream mid-backlog lets it jump ahead", async () => {
		const sink = new GatedSink();
		const sched = new SendScheduler(sink);
		sched.setSendOrder(1n, 0);
		sched.setSendOrder(2n, 0);
		void sched.enqueueStream(1n, frame(1));
		void sched.enqueueStream(2n, frame(2));

		await tick(5);
		sched.setSendOrder(2n, 100); // promote stream 2 while both are queued
		sink.release(2);
		await until(() => sink.written.length === 2);

		expect(sink.written).toEqual([2, 1]);
	});

	test("control is not starved by a stream flood", async () => {
		const sink = new GatedSink();
		const sched = new SendScheduler(sink);
		sched.setSendOrder(1n, 0);

		let left = 50;
		const arm = () => {
			if (left-- > 0) void sched.enqueueStream(1n, frame(1)).then(arm);
		};
		arm();
		await tick(5);

		sink.release(5); // drain a few stream frames
		await until(() => sink.written.length === 5);
		sched.enqueueControl(frame(99)); // control arrives mid-flood
		sink.release(1);
		await until(() => sink.written.length === 6);

		expect(sink.written[5]).toBe(99); // served on the very next write
	});

	test("datagrams are sent after control but ahead of stream data", async () => {
		const sink = new GatedSink();
		const sched = new SendScheduler(sink);

		// Give the transport room first, so the datagram is admitted (a datagram
		// enqueued under backpressure would be dropped — see the next test).
		sink.release(3);
		sched.setSendOrder(1n, 0);
		void sched.enqueueStream(1n, frame(10));
		sched.enqueueDatagram(frame(50));
		sched.enqueueControl(frame(99));

		await until(() => sink.written.length === 3);
		expect(sink.written).toEqual([99, 50, 10]); // control, then datagram, then stream
	});

	test("a datagram enqueued under transport backpressure is dropped", async () => {
		const sink = new GatedSink();
		const sched = new SendScheduler(sink);

		// Sink is gated (no room): the datagram is shed on enqueue...
		sched.enqueueDatagram(frame(50));
		// ...but a control frame still queues (lossless) and flushes once released.
		sched.enqueueControl(frame(99));

		sink.release(5);
		await until(() => sink.written.length === 1);
		await tick(3); // give any wrongly-queued datagram a chance to appear
		expect(sink.written).toEqual([99]); // datagram dropped, control sent
	});

	test("datagrams are dropped once the lane is full even with room", async () => {
		const sink = new GatedSink();
		const sched = new SendScheduler(sink);

		// Plenty of room, so wantsMore() stays true across the synchronous burst;
		// the lane cap (DATAGRAM_QUEUE_LIMIT = 1024) is what sheds the excess.
		sink.release(2000);
		for (let i = 0; i < 1030; i++) sched.enqueueDatagram(frame(1));

		await until(() => sink.written.length === 1024, 500);
		await tick(3);
		expect(sink.written.length).toBe(1024);
	});

	test("dropStream discards queued data and rejects its promise", async () => {
		const sink = new GatedSink();
		const sched = new SendScheduler(sink);
		sched.setSendOrder(1n, 0);

		const p = sched.enqueueStream(1n, frame(1));
		const rejected = p.then(
			() => "resolved",
			() => "rejected",
		);
		sched.dropStream(1n, new Error("reset"));
		sink.release(5);
		await tick(10);

		expect(await rejected).toBe("rejected");
		expect(sink.written).toEqual([]); // nothing written
	});

	test("close drops everything queued and resolves once the final frame is written", async () => {
		const sink = new GatedSink();
		const sched = new SendScheduler(sink);
		sched.setSendOrder(1n, 0);

		const p = sched.enqueueStream(1n, frame(1));
		const settled = p.then(
			() => "resolved",
			() => "rejected",
		);
		sched.enqueueControl(frame(98));
		let flushed = false;
		const closed = sched.close(new Error("closing"), frame(99)).then(() => {
			flushed = true;
		});

		await tick(5);
		expect(flushed).toBe(false); // the sink hasn't taken it yet

		sink.release(5);
		await closed;

		expect(await settled).toBe("rejected");
		expect(sink.written).toEqual([99]); // only the final frame
	});

	test("a failed write rejects that stream's promise and tears the scheduler down", async () => {
		const sink = new GatedSink();
		const sched = new SendScheduler(sink);
		sched.setSendOrder(1n, 0);
		sched.setSendOrder(2n, 0);

		const first = sched.enqueueStream(1n, frame(1));
		const firstSettled = first.then(
			() => "resolved",
			(e) => `rejected:${e.message}`,
		);
		sink.failNext = new Error("boom");
		sink.release(1);

		// The in-flight frame's promise rejects with the write error...
		expect(await firstSettled).toBe("rejected:boom");
		// ...and the scheduler is now closed, so further enqueues reject too.
		await expect(sched.enqueueStream(2n, frame(2))).rejects.toThrow("boom");
		expect(sink.written).toEqual([]);
	});

	test("enqueueStream rejects a duplicate frame for the same stream", async () => {
		const sink = new GatedSink();
		const sched = new SendScheduler(sink);
		sched.setSendOrder(1n, 0);

		const first = sched.enqueueStream(1n, frame(1));
		// Second enqueue for the same stream before the first is serviced.
		await expect(sched.enqueueStream(1n, frame(2))).rejects.toThrow("already has a queued frame");

		// The original frame is untouched and still delivers.
		sink.release(1);
		await first;
		expect(sink.written).toEqual([1]);
	});

	test("onActivity fires once per written frame", async () => {
		const sink = new GatedSink();
		let activity = 0;
		const sched = new SendScheduler(sink, { onActivity: () => activity++ });
		sched.setSendOrder(1n, 0);
		void sched.enqueueStream(1n, frame(1));
		sched.enqueueControl(frame(99));

		await tick(5);
		sink.release(2);
		await until(() => sink.written.length === 2);

		expect(activity).toBe(2);
	});
});
