import { expect, test } from "bun:test";
import { Signal } from "@moq/signals";
import { Allocator, allocate, Viewers, type Want } from "./bandwidth.ts";
import { Producer as TrackProducer } from "./track.ts";
import { Observed } from "./util/observed.ts";

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

// Several players on one connection must not each select against the whole downlink:
// the receive estimate splits max-min fair by each viewer's ceiling, and never sums above it.
test("viewers split the receive estimate max-min fair", () => {
	const estimate = new Signal<number | undefined>(6_000_000);
	const viewers = new Viewers(estimate);

	const small = viewers.watch(1_000_000);
	const large = viewers.watch(8_000_000);
	expect(small.peek()).toBe(1_000_000);
	expect(large.peek()).toBe(5_000_000);

	// Three even claims on a link too small for any of them: an even cut each, never above it.
	estimate.set(2_000_000);
	small.update(4_000_000);
	large.update(4_000_000);
	const third = viewers.watch(4_000_000);
	const grants = [small.peek() ?? 0, large.peek() ?? 0, third.peek() ?? 0];
	for (const grant of grants) expect(Math.abs(grant - 666_666)).toBeLessThanOrEqual(1);
	expect(grants.reduce((sum, grant) => sum + grant, 0)).toBe(2_000_000);

	// No estimate is "hold your selection", not zero.
	estimate.set(undefined);
	expect(small.peek()).toBeUndefined();

	viewers.close();
});

test("the viewers' target sums their ceilings until overridden", () => {
	const viewers = new Viewers(new Signal<number | undefined>(1_000_000));
	expect(viewers.target.peek()).toBe(0);

	const first = viewers.watch(1_000_000);
	const second = viewers.watch(2_000_000);
	expect(viewers.target.peek()).toBe(3_000_000);

	second.update(4_000_000);
	expect(viewers.target.peek()).toBe(5_000_000);

	// Zero turns probing off; clearing the override follows the viewers again.
	viewers.setTarget(0);
	expect(viewers.target.peek()).toBe(0);
	viewers.setTarget(10_000_000);
	expect(viewers.target.peek()).toBe(10_000_000);
	viewers.setTarget(undefined);
	expect(viewers.target.peek()).toBe(5_000_000);

	first.close();
	expect(viewers.target.peek()).toBe(4_000_000);
	second.close();
	expect(viewers.target.peek()).toBe(0);

	viewers.close();
});

test("a closed viewers registry claims nothing", () => {
	const viewers = new Viewers(new Signal<number | undefined>(1_000_000));
	viewers.close();

	const viewer = viewers.watch(2_000_000);
	expect(viewers.target.peek()).toBe(0);
	expect(viewer.peek()).toBeUndefined();
	viewer.close();
});

// Observing a connection's receive estimate is what requests PROBE, so a registry with
// nobody watching must not hold that demand.
test("an idle viewers registry does not observe the estimate", async () => {
	const estimate = new Observed<number | undefined>(1_000_000);
	const viewers = new Viewers(estimate);
	expect(estimate.wanted.peek()).toBe(false);

	const viewer = viewers.watch(1_000_000);
	await settled();
	expect(estimate.wanted.peek()).toBe(true);

	viewer.close();
	await settled();
	expect(estimate.wanted.peek()).toBe(false);

	viewers.close();
});

// Let queued signal reactions run without relying on host timers.
function settled(): Promise<void> {
	return new Promise((resolve) => {
		const channel = new MessageChannel();
		channel.port1.onmessage = () => {
			channel.port1.close();
			channel.port2.close();
			resolve();
		};
		channel.port2.postMessage(null);
	});
}
