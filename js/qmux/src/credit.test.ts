import { describe, expect, test } from "bun:test";
import { Credit, StreamLimit, Window } from "./credit.ts";
import { VarInt } from "./varint.ts";

/** Settle a claim() to its value or its rejection error, without leaking an
 *  unhandled rejection. */
function settle<T>(p: Promise<T>): Promise<T | Error> {
	return p.then(
		(v) => v,
		(e) => e,
	);
}

describe("Credit basics still work", () => {
	test("claim() resolves and clamps to remaining credit", async () => {
		const credit = new Credit(10n);
		expect(await credit.claim(4n)).toBe(4n);
		expect(await credit.claim(100n)).toBe(6n);
	});

	test("zero-limit claim resolves to 0n even after close", async () => {
		const credit = new Credit(0n);
		credit.close(new Error("Connection closed: 0 "));
		expect(await credit.claim(0n)).toBe(0n);
	});
});

describe("Credit.claim abort", () => {
	test("aborting a parked claim rejects with the reason and leaves no waiter", async () => {
		const credit = new Credit(0n);
		const controller = new AbortController();
		const parked = settle(credit.claim(1n, controller.signal));

		const reason = new Error("aborted");
		controller.abort(reason);
		expect(await parked).toBe(reason);

		// The aborted claim took nothing: a later grant goes to the next claimant.
		const next = credit.claim(5n);
		credit.increaseMax(5n);
		expect(await next).toBe(5n);
	});

	test("an already-aborted signal rejects without claiming available credit", async () => {
		const credit = new Credit(10n);
		const reason = new Error("aborted");
		expect(await settle(credit.claim(1n, AbortSignal.abort(reason)))).toBe(reason);
		expect(credit.available).toBe(10n);
	});
});

describe("Credit.close reason propagation", () => {
	test("claim() after close(reason) rejects with the exact reason instance", async () => {
		const credit = new Credit(0n);
		const reason = new Error("Connection closed: 42 bye");
		credit.close(reason);

		expect(await settle(credit.claim(1n))).toBe(reason);
	});

	test("a claim() pending at close(reason) rejects with the exact reason instance", async () => {
		const credit = new Credit(0n); // no credit available -> claim parks on a waiter
		const reason = new Error("Connection closed: 7 gone");

		const settled = settle(credit.claim(1n));
		credit.close(reason);
		expect(await settled).toBe(reason);
	});

	test("close() with no reason rejects with a generic Error('closed')", async () => {
		const credit = new Credit(0n);
		const pending = settle(credit.claim(1n));
		credit.close();

		const pendingErr = await pending;
		expect(pendingErr).toBeInstanceOf(Error);
		expect((pendingErr as Error).message).toBe("closed");

		// A claim() made *after* a reason-less close also gets the generic Error.
		const afterErr = await settle(credit.claim(1n));
		expect(afterErr).toBeInstanceOf(Error);
		expect((afterErr as Error).message).toBe("closed");
	});

	test("first close(reason) wins; later close() calls do not overwrite it", async () => {
		const credit = new Credit(0n);
		const first = new Error("Connection closed: 1 first");
		credit.close(first);
		credit.close(new Error("Connection closed: 2 second"));
		credit.close(); // reason-less close after a reasoned one

		expect(await settle(credit.claim(1n))).toBe(first);
	});
});

describe("Credit.wait abort", () => {
	test("an abort after the wake but before the wait resumes rejects", async () => {
		const credit = new Credit(0n);
		const controller = new AbortController();
		const reason = new Error("stopped");
		const wait = settle(credit.wait(controller.signal));
		credit.increaseMax(10n);
		controller.abort(reason);
		expect(await wait).toBe(reason);
	});
});

describe("Window", () => {
	test("holds the peer to the limit it was sent, not one still pending", () => {
		const window = new Window(100n, () => {});
		window.receiveTo(60n);
		window.release(60n);
		expect(window.max).toBe(160n);
		expect(window.allows(120n)).toBe(false);
		expect(window.takePending()).toBe(true);
		expect(window.allows(120n)).toBe(true);
	});

	test("never advertises past a varint", () => {
		const window = new Window(VarInt.MAX, () => {});
		window.receiveTo(VarInt.MAX);
		window.release(VarInt.MAX);
		// Already at the largest limit a frame can carry, so no update is owed.
		expect(window.takePending()).toBe(false);
		expect(window.max).toBe(VarInt.MAX);
	});
});

describe("StreamLimit", () => {
	test("a slot held back returns once the peer opens up to the limit", () => {
		let pending = 0;
		const limit = new StreamLimit(10n, () => pending++);
		expect(limit.open(1n)).toBe(true);
		limit.close();
		// Plenty of headroom, so the slot waits.
		expect(pending).toBe(0);
		expect(limit.open(9n)).toBe(true);
		expect(pending).toBe(1);
		expect(limit.takePending()).toBe(true);
		expect(limit.max).toBe(11n);
	});

	test("holds the peer to the limit it was sent, not one still pending", () => {
		const limit = new StreamLimit(1n, () => {});
		expect(limit.open(0n)).toBe(true);
		limit.close();
		expect(limit.max).toBe(2n);
		expect(limit.open(1n)).toBe(false);
		expect(limit.takePending()).toBe(true);
		expect(limit.open(1n)).toBe(true);
	});
});
