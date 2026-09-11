import { expect, test } from "bun:test";
import { Signal } from "@moq/signals";
import { Allocator, allocate, type Want } from "./bandwidth.ts";
import { Producer as TrackProducer } from "./track.ts";

/** Priorities matching hang's, which is what the allocator sees in practice. */
const AUDIO = 80;
const VIDEO = 60;

function want(id: number, priority: number, max: number): Want {
	return { id, priority, max };
}

function track(priority: number): TrackProducer {
	return new TrackProducer("t").accept({ priority });
}

test("strict priority fills the top tier first", () => {
	const wants = [want(0, AUDIO, 128_000), want(1, VIDEO, 4_000_000)];

	// Audio takes its reservation off the top; video gets what's left. This is
	// the job `rate::Policy::headroom` used to approximate with a flat 10%.
	expect(allocate(2_000_000, wants, 0)).toBe(128_000);
	expect(allocate(2_000_000, wants, 1)).toBe(1_872_000);
});

test("a starved tier gets nothing", () => {
	const wants = [want(0, AUDIO, 2_000_000), want(1, VIDEO, 4_000_000)];

	expect(allocate(1_000_000, wants, 0)).toBe(1_000_000);
	// Strict, not weighted: the lower tier is not owed a floor.
	expect(allocate(1_000_000, wants, 1)).toBe(0);
});

test("one tier still serves audio before video", () => {
	const flat = [want(0, 0, 128_000), want(1, 0, 4_000_000)];
	const tiered = [want(0, AUDIO, 128_000), want(1, VIDEO, 4_000_000)];

	for (const wants of [flat, tiered]) {
		expect(allocate(2_000_000, wants, 0)).toBe(128_000);
		expect(allocate(2_000_000, wants, 1)).toBe(1_872_000);
	}
});

test("an even tier splits evenly", () => {
	const wants = [want(0, VIDEO, 4_000_000), want(1, VIDEO, 4_000_000)];

	expect(allocate(6_000_000, wants, 0)).toBe(3_000_000);
	expect(allocate(6_000_000, wants, 1)).toBe(3_000_000);
});

test("a small share frees what it does not want", () => {
	const wants = [want(0, VIDEO, 1_000_000), want(1, VIDEO, 8_000_000)];

	expect(allocate(6_000_000, wants, 0)).toBe(1_000_000);
	expect(allocate(6_000_000, wants, 1)).toBe(5_000_000);
});

test("surplus is left unclaimed", () => {
	expect(allocate(10_000_000, [want(0, VIDEO, 4_000_000)], 0)).toBe(4_000_000);
});

test("an unregistered share has no grant", () => {
	expect(allocate(1_000_000, [], 0)).toBeUndefined();
	expect(allocate(1_000_000, [want(0, VIDEO, 1_000)], 7)).toBeUndefined();
});

test("concurrent tracks split the estimate", () => {
	const estimate = new Signal<number | undefined>(undefined);
	const allocator = new Allocator(estimate);

	const firstTrack = track(VIDEO);
	firstTrack.subscribe();
	const first = allocator.reserve(firstTrack, 4_000_000);

	estimate.set(2_000_000);
	// Alone, it gets everything it asked for that the link can carry.
	expect(first.peek()).toBe(2_000_000);

	const secondTrack = track(VIDEO);
	secondTrack.subscribe();
	const second = allocator.reserve(secondTrack, 4_000_000);

	expect(first.peek()).toBe(1_000_000);
	expect(second.peek()).toBe(1_000_000);

	allocator.close();
});

test("an idle track claims nothing", () => {
	const estimate = new Signal<number | undefined>(2_000_000);
	const allocator = new Allocator(estimate);

	const watchedTrack = track(VIDEO);
	watchedTrack.subscribe();
	const watched = allocator.reserve(watchedTrack, 4_000_000);

	const idleTrack = track(VIDEO);
	const idle = allocator.reserve(idleTrack, 4_000_000);

	expect(watched.peek()).toBe(2_000_000);
	// Not zero: an idle share reports "no opinion" so a sender that is mid-shutdown
	// holds its rate instead of retuning to the floor on the way out.
	expect(idle.peek()).toBeUndefined();

	idleTrack.subscribe();
	expect(watched.peek()).toBe(1_000_000);
	expect(idle.peek()).toBe(1_000_000);

	allocator.close();
});

test("a share wakes when a sibling goes idle", async () => {
	const estimate = new Signal<number | undefined>(2_000_000);
	const allocator = new Allocator(estimate);

	const mineTrack = track(VIDEO);
	mineTrack.subscribe();
	const mine = allocator.reserve(mineTrack, 4_000_000);

	const siblingTrack = track(VIDEO);
	const siblingSub = siblingTrack.subscribe();
	const sibling = allocator.reserve(siblingTrack, 4_000_000);

	expect(mine.peek()).toBe(1_000_000);
	expect(sibling.peek()).toBe(1_000_000);

	const next = mine.grant.changed();
	siblingSub.close();
	expect(await next).toBe(2_000_000);
	expect(mine.peek()).toBe(2_000_000);

	allocator.close();
});

test("an unchanged slice still follows the estimate past the cap", async () => {
	const estimate = new Signal<number | undefined>(undefined);
	const allocator = new Allocator(estimate);

	const producer = track(VIDEO);
	producer.subscribe();
	const reserved = allocator.reserve(producer, 4_000_000);

	estimate.set(10_000_000);
	expect(reserved.peek()).toBe(4_000_000);

	const next = reserved.grant.changed();
	// Still miles above the reservation, so the slice holds at 4 Mbps.
	estimate.set(9_000_000);
	expect(reserved.peek()).toBe(4_000_000);

	// The estimate finally drops past the reservation: this has to reach it.
	estimate.set(1_000_000);
	expect(await next).toBe(1_000_000);
	expect(reserved.peek()).toBe(1_000_000);

	allocator.close();
});

test("a parked share is woken by sibling demand", async () => {
	const estimate = new Signal<number | undefined>(2_000_000);
	const allocator = new Allocator(estimate);

	const mineTrack = track(VIDEO);
	mineTrack.subscribe();
	const reserved = allocator.reserve(mineTrack, 4_000_000);

	const siblingTrack = track(VIDEO);
	const siblingSub = siblingTrack.subscribe();
	const sibling = allocator.reserve(siblingTrack, 4_000_000);

	expect(reserved.peek()).toBe(1_000_000);
	expect(sibling.peek()).toBe(1_000_000);

	const next = reserved.grant.changed();
	siblingSub.close();
	expect(await next).toBe(2_000_000);

	allocator.close();
});

test("a closed track is pruned", () => {
	const estimate = new Signal<number | undefined>(2_000_000);
	const allocator = new Allocator(estimate);

	const first = track(VIDEO);
	first.subscribe();
	const firstShare = allocator.reserve(first, 4_000_000);
	first.close();

	const second = track(VIDEO);
	second.subscribe();
	const secondShare = allocator.reserve(second, 4_000_000);

	expect(firstShare.peek()).toBeUndefined();
	expect(secondShare.peek()).toBe(2_000_000);

	allocator.close();
});

test("closing a reservation releases it", async () => {
	const estimate = new Signal<number | undefined>(2_000_000);
	const allocator = new Allocator(estimate);

	const firstTrack = track(VIDEO);
	firstTrack.subscribe();
	const first = allocator.reserve(firstTrack, 4_000_000);

	const secondTrack = track(VIDEO);
	secondTrack.subscribe();
	const second = allocator.reserve(secondTrack, 4_000_000);
	expect(second.peek()).toBe(1_000_000);

	const orphan = first.grant;
	expect(orphan.peek()).toBe(1_000_000);

	const next = orphan.changed();
	first.close();
	expect(second.peek()).toBe(2_000_000);
	expect(await next).toBeUndefined();
	expect(orphan.peek()).toBeUndefined();
	expect(first.peek()).toBeUndefined();

	allocator.close();
});

test("update changes the claim in place", () => {
	const estimate = new Signal<number | undefined>(6_000_000);
	const allocator = new Allocator(estimate);

	const smallTrack = track(VIDEO);
	smallTrack.subscribe();
	const small = allocator.reserve(smallTrack, 1_000_000);

	const largeTrack = track(VIDEO);
	largeTrack.subscribe();
	const large = allocator.reserve(largeTrack, 8_000_000);

	expect(small.peek()).toBe(1_000_000);
	expect(large.peek()).toBe(5_000_000);

	small.update(4_000_000);
	expect(small.peek()).toBe(3_000_000);
	expect(large.peek()).toBe(3_000_000);

	small.update(1_000_000);
	expect(large.peek()).toBe(5_000_000);

	allocator.close();
});

test("update wakes a parked reader", async () => {
	const estimate = new Signal<number | undefined>(6_000_000);
	const allocator = new Allocator(estimate);

	const producer = track(VIDEO);
	producer.subscribe();
	const reserved = allocator.reserve(producer, 1_000_000);
	expect(reserved.peek()).toBe(1_000_000);

	const next = reserved.grant.changed();
	reserved.update(4_000_000);
	expect(await next).toBe(4_000_000);

	allocator.close();
});

test("unlimited reservations never claim", () => {
	const allocator = Allocator.unlimited();
	const producer = track(VIDEO);
	producer.subscribe();
	const reserved = allocator.reserve(producer, 4_000_000);
	expect(reserved.peek()).toBeUndefined();
	allocator.close();
});

test("a closed allocator reports no grant", () => {
	const estimate = new Signal<number | undefined>(2_000_000);
	const allocator = new Allocator(estimate);
	const producer = track(VIDEO);
	producer.subscribe();
	const reserved = allocator.reserve(producer, 4_000_000);
	expect(reserved.peek()).toBe(2_000_000);

	allocator.close();
	expect(reserved.peek()).toBeUndefined();
	expect(reserved.grant.peek()).toBeUndefined();
});

test("reserve and update reject a non-finite or negative ceiling", () => {
	const allocator = new Allocator(new Signal<number | undefined>(2_000_000));
	const producer = track(VIDEO);
	producer.subscribe();

	for (const max of [Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY, -1]) {
		expect(() => allocator.reserve(producer, max)).toThrow(/finite non-negative/);
	}

	const reserved = allocator.reserve(producer, 1_000_000);
	for (const max of [Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY, -1]) {
		expect(() => reserved.update(max)).toThrow(/finite non-negative/);
	}
	expect(reserved.peek()).toBe(1_000_000);

	allocator.close();
});
