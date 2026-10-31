import { expect, test } from "bun:test";
import { Signal } from "@moq/signals";
import { Allocator, allocate, follow, type Want } from "./bandwidth.ts";
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
	const first = allocator.reserve(firstTrack.demand(), 4_000_000);

	estimate.set(2_000_000);
	// Alone, it gets everything it asked for that the link can carry.
	expect(first.peek()).toBe(2_000_000);

	const secondTrack = track(VIDEO);
	secondTrack.subscribe();
	const second = allocator.reserve(secondTrack.demand(), 4_000_000);

	expect(first.peek()).toBe(1_000_000);
	expect(second.peek()).toBe(1_000_000);

	allocator.close();
});

test("an idle track claims nothing", async () => {
	const estimate = new Signal<number | undefined>(2_000_000);
	const allocator = new Allocator(estimate);

	const watchedTrack = track(VIDEO);
	watchedTrack.subscribe();
	const watched = allocator.reserve(watchedTrack.demand(), 4_000_000);

	const idleTrack = track(VIDEO);
	const idle = allocator.reserve(idleTrack.demand(), 4_000_000);

	expect(watched.peek()).toBe(2_000_000);
	// What it would get if a viewer joined, so a rendition stopped for lack of
	// bandwidth can resume after its viewers left.
	expect(idle.peek()).toBe(1_000_000);
	expect(idle.rate.peek()).toBe(1_000_000);

	// The idle share keeps following the estimate.
	const next = idle.rate.changed();
	estimate.set(4_000_000);
	expect(await next).toBe(2_000_000);
	expect(watched.peek()).toBe(4_000_000);

	estimate.set(2_000_000);
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
	const mine = allocator.reserve(mineTrack.demand(), 4_000_000);

	const siblingTrack = track(VIDEO);
	const siblingSub = siblingTrack.subscribe();
	const sibling = allocator.reserve(siblingTrack.demand(), 4_000_000);

	expect(mine.peek()).toBe(1_000_000);
	expect(sibling.peek()).toBe(1_000_000);

	const next = mine.rate.changed();
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
	const reserved = allocator.reserve(producer.demand(), 4_000_000);

	estimate.set(10_000_000);
	expect(reserved.peek()).toBe(4_000_000);

	const next = reserved.rate.changed();
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
	const reserved = allocator.reserve(mineTrack.demand(), 4_000_000);

	const siblingTrack = track(VIDEO);
	const siblingSub = siblingTrack.subscribe();
	const sibling = allocator.reserve(siblingTrack.demand(), 4_000_000);

	expect(reserved.peek()).toBe(1_000_000);
	expect(sibling.peek()).toBe(1_000_000);

	const next = reserved.rate.changed();
	siblingSub.close();
	expect(await next).toBe(2_000_000);

	allocator.close();
});

test("a closed track is pruned", () => {
	const estimate = new Signal<number | undefined>(2_000_000);
	const allocator = new Allocator(estimate);

	const first = track(VIDEO);
	first.subscribe();
	const firstShare = allocator.reserve(first.demand(), 4_000_000);
	first.finish();
	first.close();

	const second = track(VIDEO);
	second.subscribe();
	const secondShare = allocator.reserve(second.demand(), 4_000_000);

	expect(firstShare.peek()).toBeUndefined();
	expect(secondShare.peek()).toBe(2_000_000);

	allocator.close();
});

test("closing a reservation releases it", async () => {
	const estimate = new Signal<number | undefined>(2_000_000);
	const allocator = new Allocator(estimate);

	const firstTrack = track(VIDEO);
	firstTrack.subscribe();
	const first = allocator.reserve(firstTrack.demand(), 4_000_000);

	const secondTrack = track(VIDEO);
	secondTrack.subscribe();
	const second = allocator.reserve(secondTrack.demand(), 4_000_000);
	expect(second.peek()).toBe(1_000_000);

	const orphan = first.rate;
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
	const small = allocator.reserve(smallTrack.demand(), 1_000_000);

	const largeTrack = track(VIDEO);
	largeTrack.subscribe();
	const large = allocator.reserve(largeTrack.demand(), 8_000_000);

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
	const reserved = allocator.reserve(producer.demand(), 1_000_000);
	expect(reserved.peek()).toBe(1_000_000);

	const next = reserved.rate.changed();
	reserved.update(4_000_000);
	expect(await next).toBe(4_000_000);

	allocator.close();
});

test("unlimited reservations never claim", () => {
	const allocator = Allocator.unlimited();
	const producer = track(VIDEO);
	producer.subscribe();
	const reserved = allocator.reserve(producer.demand(), 4_000_000);
	expect(reserved.peek()).toBeUndefined();
	allocator.close();
});

test("a closed allocator reports no grant", () => {
	const estimate = new Signal<number | undefined>(2_000_000);
	const allocator = new Allocator(estimate);
	const producer = track(VIDEO);
	producer.subscribe();
	const reserved = allocator.reserve(producer.demand(), 4_000_000);
	expect(reserved.peek()).toBe(2_000_000);

	allocator.close();
	expect(reserved.peek()).toBeUndefined();
	expect(reserved.rate.peek()).toBeUndefined();
});

test("reserve and update reject a non-finite or negative ceiling", () => {
	const allocator = new Allocator(new Signal<number | undefined>(2_000_000));
	const producer = track(VIDEO);
	producer.subscribe();

	for (const max of [Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY, -1]) {
		expect(() => allocator.reserve(producer.demand(), max)).toThrow(/finite non-negative/);
	}

	const reserved = allocator.reserve(producer.demand(), 1_000_000);
	for (const max of [Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY, -1]) {
		expect(() => reserved.update(max)).toThrow(/finite non-negative/);
	}
	expect(reserved.peek()).toBe(1_000_000);

	allocator.close();
});

test("the target sums every reservation, demanded or not", async () => {
	const allocator = Allocator.unlimited();
	const target = follow(allocator);
	expect(target.rate.peek()).toBe(0);

	const watchedTrack = track(VIDEO);
	watchedTrack.subscribe();
	const watched = allocator.reserve(watchedTrack.demand(), 4_000_000);
	expect(target.rate.peek()).toBe(4_000_000);

	// Nobody watches this one, which is exactly the rung that must still be funded.
	const idleTrack = track(VIDEO);
	const idle = allocator.reserve(idleTrack.demand(), 1_000_000);
	expect(target.rate.peek()).toBe(5_000_000);

	idle.update(2_000_000);
	expect(target.rate.peek()).toBe(6_000_000);

	watched.close();
	expect(target.rate.peek()).toBe(2_000_000);

	// A closed track's claim leaves with it, even while its reservation is still held.
	const next = target.rate.changed();
	idleTrack.close();
	expect(await next).toBe(0);

	target.close();
	allocator.close();
});

test("an override replaces the sum until it is cleared", () => {
	const allocator = Allocator.unlimited();
	const target = follow(allocator);
	const producer = track(VIDEO);
	allocator.reserve(producer.demand(), 4_000_000);
	expect(target.rate.peek()).toBe(4_000_000);

	allocator.setTarget(0);
	expect(target.rate.peek()).toBe(0);
	allocator.setTarget(10_000_000);
	expect(target.rate.peek()).toBe(10_000_000);
	allocator.setTarget(undefined);
	expect(target.rate.peek()).toBe(4_000_000);

	expect(() => allocator.setTarget(-1)).toThrow(/finite non-negative/);

	target.close();
	allocator.close();
});

test("a reservation is probed while a session follows a target above zero", () => {
	const allocator = new Allocator(new Signal<number | undefined>(2_000_000));
	const producer = track(VIDEO);
	producer.subscribe();
	const reserved = allocator.reserve(producer.demand(), 4_000_000);
	expect(reserved.probed.peek()).toBe(false);

	const target = follow(allocator);
	expect(reserved.probed.peek()).toBe(true);

	// A zero target turns probing off, though the session still follows it.
	allocator.setTarget(0);
	expect(reserved.probed.peek()).toBe(false);
	allocator.setTarget(undefined);
	expect(reserved.probed.peek()).toBe(true);

	target.close();
	target.close();
	expect(reserved.probed.peek()).toBe(false);

	const again = follow(allocator);
	reserved.close();
	expect(reserved.probed.peek()).toBe(false);

	again.close();
	allocator.close();
});
