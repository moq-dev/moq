import { expect, test } from "bun:test";
import { yieldNow } from "@moq/clock";
import { type Getter, Signal } from "@moq/signals";
import type { Grant } from "./auth.ts";
import { type AuthSession, enforceGrant } from "./auth_session.ts";
import * as Path from "./path.ts";

/** How many reactions `run` attached to each promise, by `then` on it. */
async function reactions(run: () => Promise<void>): Promise<number[]> {
	const then = Promise.prototype.then;
	const counts = new Map<Promise<unknown>, number>();
	// biome-ignore lint/suspicious/noThenProperty: counts reactions on every promise.
	Promise.prototype.then = function (this: Promise<unknown>, ...args: Parameters<typeof then>) {
		counts.set(this, (counts.get(this) ?? 0) + 1);
		return then.apply(this, args);
	} as typeof then;
	try {
		await run();
	} finally {
		// biome-ignore lint/suspicious/noThenProperty: restores the original.
		Promise.prototype.then = then;
	}
	return [...counts.values()];
}

test.each([2, 64])("churn over %i live broadcasts leaves no reaction behind per change", async (live) => {
	const everything = new Path.Patterns([Path.Pattern.subtree("")]);
	const counts = await reactions(async () => {
		const { promise: closed, resolve: close } = Promise.withResolvers<void>();
		const quic = { closed, close: () => {} } as unknown as WebTransport;
		const advertised = new Signal<ReadonlyMap<Path.Valid, unknown> | undefined>(new Map());
		const grant = new Signal<Grant | undefined>({ publish: everything, subscribe: everything });
		const enforcing = enforceGrant({ quic, advertised, grant, setupAnswered: Promise.resolve() });
		// A live table that keeps changing: one broadcast swaps out per update.
		for (let i = 0; i < 1_000; i++) {
			advertised.set(new Map(Array.from({ length: live }, (_, j) => [Path.from(`live/${i + j}`), i])));
			await yieldNow();
		}
		close();
		await enforcing;
	});
	expect(Math.max(...counts)).toBeLessThan(10);
});

test("a session that closes during an authorization wait ends enforcement without closing it again", async () => {
	const { promise: closed, resolve: close } = Promise.withResolvers<void>();
	let closes = 0;
	const quic = { closed, close: () => closes++ } as unknown as WebTransport;
	// Nothing granted yet, and an authorization that is never answered.
	const grant = new Signal<Grant | undefined>({ publish: new Path.Patterns(), subscribe: new Path.Patterns() });
	const table = new Signal<ReadonlyMap<Path.Valid, unknown> | undefined>(new Map([[Path.from("live"), 0]]));
	// Counts the change callbacks the enforcer holds on the table, which must go with it.
	let registered = 0;
	const advertised = {
		peek: () => table.peek(),
		changed: (fn: (value: ReadonlyMap<Path.Valid, unknown> | undefined) => void) => {
			registered++;
			const dispose = table.changed(fn);
			return () => {
				registered--;
				dispose();
			};
		},
	} as unknown as Getter<ReadonlyMap<Path.Valid, unknown> | undefined>;
	let waiting = 0;
	const auth = {
		ready: (_path: Path.Valid, _direction: string, { signal }: { signal?: AbortSignal } = {}) =>
			new Promise<boolean>((_, reject) => {
				waiting++;
				signal?.addEventListener("abort", () => reject(signal.reason), { once: true });
			}),
	} as unknown as AuthSession;

	let done = false;
	const enforcing = enforceGrant({ quic, advertised, grant, setupAnswered: Promise.resolve(), auth }).then(() => {
		done = true;
	});
	for (let i = 0; i < 10 && waiting === 0; i++) await yieldNow();
	expect(waiting).toBe(1);

	close();
	for (let i = 0; i < 10 && !done; i++) await yieldNow();
	expect(done).toBe(true);
	expect(closes).toBe(0);
	await enforcing;
	expect(registered).toBe(0);
});
