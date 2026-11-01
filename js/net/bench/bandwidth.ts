/**
 * Sweep what dividing a connection's estimate costs as its reservations fan out.
 *
 * Each shape is connections x reservations, priority tiers, idle reservations and
 * observers per reservation. Each change (every connection's estimate, one track's demand
 * or one ceiling per connection) is timed until the allocator republishes and every
 * observer heard it; peek reads every reservation once, as a sender does per frame.
 * Every grant is checked against a reference division before and after timing.
 */
import { Signal } from "@moq/signals";
import { Allocator, type Reservation } from "../src/bandwidth.ts";
import { Producer as TrackProducer, type Subscriber } from "../src/track.ts";

interface Shape {
	connections: number;
	reservations: number;
	tiers: number;
	idle: boolean;
	observers: number;
}

const shape = (reservations: number, tiers: number, extra: Partial<Shape> = {}): Shape => ({
	connections: 1,
	reservations,
	tiers,
	idle: false,
	observers: 1,
	...extra,
});

/** Reservations alone, then distinct priorities, then each other axis on its own. */
const SHAPES: Shape[] = [
	shape(1, 1),
	shape(16, 1),
	shape(64, 1),
	shape(256, 1),
	shape(16, 16),
	shape(64, 64),
	shape(256, 256),
	shape(64, 4),
	shape(64, 1, { idle: true }),
	shape(64, 1, { observers: 4 }),
	shape(64, 1, { connections: 4 }),
];

/** The two per-reservation estimates an estimate change alternates between, in kbps. */
const ESTIMATES = [700, 450];

/** Five sizes, so tiers hold ties as well as distinct ceilings. */
const ceiling = (index: number) => (500 + 250 * (index % 5)) * 1000;

interface Slot {
	track: TrackProducer;
	subscriber: Subscriber | undefined;
	reservation: Reservation;
	priority: number;
	max: number;
}

class Connection {
	readonly estimate: Signal<number | undefined>;
	readonly allocator: Allocator;
	readonly slots: Slot[] = [];
	heard = 0;

	constructor(shape: Shape) {
		this.estimate = new Signal<number | undefined>(ESTIMATES[0] * 1000 * shape.reservations);
		this.allocator = new Allocator(this.estimate);
		for (let index = 0; index < shape.reservations; index++) {
			// Shifted into -128..=127, which keeps up to 256 tiers distinct.
			const priority = (index % shape.tiers) - 128;
			const track = new TrackProducer(`t${index}`).accept({ priority });
			const subscriber = shape.idle && index % 2 === 1 ? undefined : track.subscribe();
			const max = ceiling(index);
			const reservation = this.allocator.reserve(track.demand(), max);
			for (let observer = 0; observer < shape.observers; observer++) {
				reservation.rate.subscribe(() => this.heard++);
			}
			this.slots.push({ track, subscriber, reservation, priority, max });
		}
	}

	/**
	 * What `index` should read: strict priority, max-min fair within a tier in ascending
	 * (ceiling, registration) order, an idle track claiming nothing but its own reader seeing
	 * what it would get back, served after the demanded tracks with its ceiling.
	 */
	expected(index: number): number {
		let budget = this.estimate.peek() ?? 0;
		const claims = this.slots
			.map((slot, other) => ({ ...slot, other, rejoin: !slot.subscriber }))
			.filter((claim) => claim.other === index || !claim.rejoin);
		claims.sort((a, b) => b.priority - a.priority || a.max - b.max || +a.rejoin - +b.rejoin || a.other - b.other);
		for (let start = 0; start < claims.length; ) {
			let end = start;
			while (end < claims.length && claims[end].priority === claims[start].priority) end++;
			for (let at = start; at < end; at++) {
				const grant = Math.min(claims[at].max, Math.floor(budget / (end - at)));
				if (claims[at].other === index) return grant;
				budget -= grant;
			}
			start = end;
		}
		throw new Error("the reservation always counts for itself");
	}

	check(): void {
		for (const [index, slot] of this.slots.entries()) {
			const expected = this.expected(index);
			if (slot.reservation.peek() !== expected) throw new Error(`reservation ${index} peeked ${slot.reservation.peek()}, expected ${expected}`);
			if (slot.reservation.rate.peek() !== expected) throw new Error(`reservation ${index} published ${slot.reservation.rate.peek()}, expected ${expected}`);
		}
	}

	change(op: Op): void {
		const middle = this.slots[this.slots.length >> 1];
		const index = this.slots.length >> 1;
		if (op === "estimate") {
			const per = (this.estimate.peek() ?? 0) / this.slots.length / 1000;
			this.estimate.set((per === ESTIMATES[0] ? ESTIMATES[1] : ESTIMATES[0]) * 1000 * this.slots.length);
		} else if (op === "demand") {
			if (middle.subscriber) {
				middle.subscriber.close();
				middle.subscriber = undefined;
			} else {
				middle.subscriber = middle.track.subscribe();
			}
		} else if (op === "ceiling") {
			middle.max = middle.max === ceiling(index) ? ceiling(index) * 2 : ceiling(index);
			middle.reservation.update(middle.max);
		} else {
			for (const slot of this.slots) slot.reservation.peek();
		}
	}

	close(): void {
		for (const slot of this.slots) {
			slot.reservation.close();
			slot.subscriber?.close();
			slot.track.close();
		}
		this.allocator.close();
	}
}

type Op = "estimate" | "demand" | "ceiling" | "peek";
const OPS: Op[] = ["estimate", "demand", "ceiling", "peek"];

/** The least each case runs, against Chromium's 100 µs clock without cross-origin isolation. */
const MIN_MS = 20;

/** Let the signals flush and the allocator republish. */
async function settle(): Promise<void> {
	for (let i = 0; i < 4; i++) await new Promise<void>((resolve) => queueMicrotask(resolve));
}

/** Time each operation over each shape, checking every grant around it. */
export async function run(): Promise<void> {
	console.log("connections,reservations,tiers,idle,observers,op,ns_per_op");
	for (const op of OPS) {
		for (const shape of SHAPES) {
			const connections = Array.from({ length: shape.connections }, () => new Connection(shape));
			await settle();
			for (const connection of connections) connection.check();

			// Pairs of changes, so both directions of each toggle count, until well past a browser's
			// coarsened clock.
			let iterations = 0;
			const start = performance.now();
			while (iterations < 4 || performance.now() - start < MIN_MS) {
				for (let i = 0; i < 2; i++) {
					for (const connection of connections) connection.change(op);
					if (op !== "peek") await settle();
				}
				iterations += 2;
			}
			const ns = ((performance.now() - start) * 1e6) / iterations;
			for (const connection of connections) connection.check();

			const heard = connections.reduce((sum, connection) => sum + connection.heard, 0);
			// Every estimate change moves some grant, so silence would mean nothing republished.
			if (op === "estimate" && heard === 0) throw new Error("no observer heard a change");
			console.log(
				`${shape.connections},${shape.reservations},${shape.tiers},${shape.idle ? shape.reservations >> 1 : 0},${shape.observers},${op},${ns.toFixed(0)}`,
			);
			for (const connection of connections) connection.close();
		}
	}
}

if (import.meta.main) await run();
