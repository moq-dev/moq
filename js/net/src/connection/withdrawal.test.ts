import { expect, test } from "bun:test";
import { Withdrawal } from "./withdrawal.ts";

// A failure in one loop must not strand the others mid-write: that is the same
// lost withdrawal this class exists to prevent, reached through a sibling's error.
test("a rejected withdrawal still lets its siblings finish", async () => {
	const withdrawal = new Withdrawal();
	const order: string[] = [];

	const failing = withdrawal.track(
		(async () => {
			order.push("failing");
			throw new Error("the first loop died");
		})(),
	);
	// Parked until the test releases it, so a close that returns on the first
	// rejection is caught with this loop still mid-write.
	const gate = Promise.withResolvers<void>();
	const sibling = withdrawal.track(
		(async () => {
			await gate.promise;
			order.push("sibling");
		})(),
	);

	// A macrotask turn, not a sleep: it only has to outrun a microtask rejection.
	const turn = new Promise((resolve) => setTimeout(resolve, 0));
	const closing = withdrawal.close();
	const outcome = closing.then(
		() => "resolved" as const,
		() => "rejected" as const,
	);
	expect(await Promise.race([outcome, turn.then(() => "still waiting" as const)])).toBe("still waiting");
	expect(order).toEqual(["failing"]);

	gate.resolve();
	// The first close is still parked on the sibling, so joining it now waits for
	// both to settle and then reports the failure.
	await expect(closing).rejects.toThrow("the first loop died");
	expect(order).toEqual(["failing", "sibling"]);
	await expect(failing).rejects.toThrow("the first loop died");
	await sibling;
});

test("close signals the loops and resolves when they all settle", async () => {
	const withdrawal = new Withdrawal();
	const seen: boolean[] = [];
	const task = withdrawal.track(
		(async () => {
			while (!withdrawal.closing.peek()) await Promise.resolve();
			seen.push(true);
		})(),
	);

	await withdrawal.close();
	expect(withdrawal.closing.peek()).toBe(true);
	expect(seen).toEqual([true]);
	await task;
});

test("close is idempotent and waits only for the tracked loops", async () => {
	const withdrawal = new Withdrawal();
	let ran = 0;
	const task = withdrawal.track(
		(async () => {
			await Promise.resolve();
			ran++;
		})(),
	);

	await Promise.all([withdrawal.close(), withdrawal.close()]);
	expect(ran).toBe(1);
	await task;
});
