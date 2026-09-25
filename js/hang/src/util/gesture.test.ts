import { afterEach, beforeEach, expect, test } from "bun:test";
import { Effect } from "@moq/signals";
import { unlock } from "./gesture";

// Minimal AudioContext stand-in: an EventTarget with a mutable `state` and a counting
// `resume()`. `transition` mirrors a real context firing `statechange` when its state moves.
class MockContext extends EventTarget {
	state = "suspended";
	resumeCalls = 0;

	resume(): Promise<void> {
		this.resumeCalls++;
		return Promise.resolve();
	}

	transition(state: string): void {
		this.state = state;
		this.dispatchEvent(new Event("statechange"));
	}
}

// Drain microtasks so scheduled effect (re)runs settle.
const flush = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

let originalDocument: typeof globalThis.document;

beforeEach(() => {
	originalDocument = globalThis.document;
	globalThis.document = new EventTarget() as unknown as Document;
});

afterEach(() => {
	globalThis.document = originalDocument;
});

const asContext = (ctx: MockContext) => ctx as unknown as AudioContext;

test("retries resume() on a user gesture until the context is running", async () => {
	const ctx = new MockContext();
	const effect = new Effect();
	unlock(effect, asContext(ctx));
	await flush();

	// The at-load attempt fires once. Browsers requiring a gesture reject it, but we still
	// try in case autoplay is already permitted.
	expect(ctx.resumeCalls).toBe(1);

	// The regression: a real gesture must produce a fresh resume(). Before the fix the effect
	// was edge-triggered on inputs that never changed, so no gesture ever reached resume().
	document.dispatchEvent(new Event("pointerdown"));
	expect(ctx.resumeCalls).toBe(2);

	document.dispatchEvent(new Event("keydown"));
	expect(ctx.resumeCalls).toBe(3);

	// Touch and pen only grant activation on pointerup, so a tap must retry there too.
	document.dispatchEvent(new Event("pointerup"));
	expect(ctx.resumeCalls).toBe(4);

	// Once the context is actually running, stop retrying: further gestures are no-ops.
	ctx.transition("running");
	await flush();
	document.dispatchEvent(new Event("pointerdown"));
	expect(ctx.resumeCalls).toBe(4);

	effect.close();
});

test("re-arms when Safari drops the context to interrupted", async () => {
	const ctx = new MockContext();
	const effect = new Effect();
	unlock(effect, asContext(ctx));
	await flush();

	ctx.transition("running");
	await flush();
	expect(ctx.resumeCalls).toBe(1); // only the at-load attempt so far

	// Safari can suspend a running context to its WebKit-only "interrupted" state. We must
	// re-attempt on the next gesture rather than staying silent.
	ctx.transition("interrupted");
	await flush();
	expect(ctx.resumeCalls).toBe(2); // immediate retry once no longer running

	document.dispatchEvent(new Event("pointerdown"));
	expect(ctx.resumeCalls).toBe(3);

	effect.close();
});

test("stops resuming after the effect closes", async () => {
	const ctx = new MockContext();
	const effect = new Effect();
	unlock(effect, asContext(ctx));
	await flush();

	effect.close();
	document.dispatchEvent(new Event("pointerdown"));
	expect(ctx.resumeCalls).toBe(1); // only the at-load attempt, no post-close listener
});
