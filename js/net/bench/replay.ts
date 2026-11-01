/** Sweep cached-group replay to joining subscribers, and arrivals into unread backlogs. */
import { Manual } from "@moq/clock";
import { Track } from "@moq/net";

type Order = "monotonic" | "reverse" | "mixed";
const ORDERS: Order[] = ["monotonic", "reverse", "mixed"];
type Kind = "join" | "append";
const PAYLOAD = 8;
// Arrivals timed per append row, so a sample spans enough work to read.
const APPENDS = 64;
const REPS = 5;
// Every retained group, however old: replay is the work being measured.
const SUBSCRIPTION = { groups: { start: { included: 0 } }, maxDelay: null };

// The arrival order of sequences 0..count-1: ascending, descending, or a seeded shuffle.
function arrivals(count: number, order: Order): number[] {
	const sequences = Array.from({ length: count }, (_, i) => i);
	if (order === "reverse") sequences.reverse();
	if (order === "mixed") {
		let seed = 42;
		for (let i = count - 1; i > 0; i--) {
			seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
			const j = seed % (i + 1);
			[sequences[i], sequences[j]] = [sequences[j], sequences[i]];
		}
	}
	return sequences;
}

// Write one finished group whose payload names its sequence, so delivery can be checked.
function publish(track: Track.Producer, sequence: number): void {
	const group = track.createGroup(sequence);
	const payload = new Uint8Array(PAYLOAD);
	new DataView(payload.buffer).setUint32(0, sequence);
	group.writeFrame({ payload });
	group.finish();
}

// Read everything a subscriber buffered, requiring every sequence exactly once, ascending.
function drain(subscriber: Track.Subscriber, count: number): void {
	let next = 0;
	for (let group = subscriber.tryRecvGroup(); group; group = subscriber.tryRecvGroup(), next++) {
		if (group.sequence !== next) throw new Error(`group ${group.sequence} delivered, expected ${next}`);
		const payload = group.tryReadFrame()?.payload;
		if (payload?.byteLength !== PAYLOAD || new DataView(payload.buffer, payload.byteOffset).getUint32(0) !== next)
			throw new Error(`group ${next} payload mismatch`);
	}
	if (next !== count) throw new Error(`${next} of ${count} groups delivered`);
}

type Counts = { sorts: number; elements: number; compares: number };

// Run `fn` counting Array.prototype.sort calls, the elements handed to them, and comparator visits.
function counted(fn: () => void): Counts {
	const sort = Array.prototype.sort;
	const counts = { sorts: 0, elements: 0, compares: 0 };
	Array.prototype.sort = function <T>(this: T[], compare?: (a: T, b: T) => number): T[] {
		counts.sorts++;
		counts.elements += this.length;
		if (!compare) return sort.call(this) as T[];
		return sort.call(this, (a: T, b: T) => {
			counts.compares++;
			return compare(a, b);
		}) as T[];
	};
	try {
		fn();
	} finally {
		Array.prototype.sort = sort;
	}
	return counts;
}

// Runs each measured operation through `op`.
type Operations = (op: (fn: () => void) => void) => void;

// A track retaining `groups` groups that arrived in `order`, then either `subscribers` joins that
// replay all of them, or APPENDS more arrivals into that many unread backlogs; reverse and mixed
// appends land inside the backlog. `measure` runs the operations. Every subscriber then drains,
// so skipped or reordered work fails the run.
function shape(kind: Kind, groups: number, order: Order, subscribers: number, measure: (ops: Operations) => void) {
	const total = kind === "join" ? groups : groups + APPENDS;
	const sequences = arrivals(total, order);
	const track = new Track.Producer(kind).accept();
	const readers: Track.Subscriber[] = [];
	if (kind === "append") for (let i = 0; i < subscribers; i++) readers.push(track.subscribe(SUBSCRIPTION));
	for (const sequence of sequences.slice(0, groups)) publish(track, sequence);
	measure((op) => {
		if (kind === "join") {
			for (let i = 0; i < subscribers; i++) op(() => readers.push(track.subscribe(SUBSCRIPTION)));
		} else {
			for (const sequence of sequences.slice(groups)) op(() => publish(track, sequence));
		}
	});
	for (const reader of readers) drain(reader, total);
	for (const reader of readers) reader.close();
	track.finish();
	track.close();
}

const median = (values: number[]) => values.sort((a, b) => a - b)[values.length >> 1];

/**
 * Print sort counts per shape from an untimed pass, then timing per shape: `join` rows time each
 * subscriber joining the retained groups, `append` rows each arrival into every subscriber's
 * unread backlog. The counts table precedes the timing header, so the nightly report records only
 * the timings as series. `max_us` is the slowest single operation, which held the event loop that
 * long. Timings are the median of REPS runs.
 */
export function run(): void {
	// Cache retention follows this clock, which never advances, so nothing ages out mid-row.
	const installed = new Manual().install();
	try {
		const shapes: [Kind, number, Order, number][] = [];
		for (const groups of [64, 512, 2048])
			for (const order of ORDERS)
				for (const subscribers of [1, 8])
					for (const kind of ["join", "append"] as const) shapes.push([kind, groups, order, subscribers]);

		console.log("groups,kind,order,subscribers,sorts,sorted_elements,compares");
		for (const [kind, groups, order, subscribers] of shapes) {
			let counts: Counts | undefined;
			shape(kind, groups, order, subscribers, (ops) => {
				counts = counted(() => ops((op) => op()));
			});
			if (!counts) throw new Error("shape was not counted");
			console.log(
				`${groups},${kind},${order},${subscribers},${counts.sorts},${counts.elements},${counts.compares}`,
			);
		}

		console.log("groups,kind,order,subscribers,us_per_op,max_us");
		for (const [kind, groups, order, subscribers] of shapes) {
			const perOp: number[] = [];
			const slowest: number[] = [];
			for (let rep = 0; rep < REPS; rep++) {
				let count = 0;
				let max = 0;
				let elapsed = 0;
				shape(kind, groups, order, subscribers, (ops) => {
					const start = performance.now();
					ops((op) => {
						const begin = performance.now();
						op();
						max = Math.max(max, performance.now() - begin);
						count++;
					});
					elapsed = performance.now() - start;
				});
				perOp.push((elapsed * 1000) / count);
				slowest.push(max * 1000);
			}
			console.log(
				`${groups},${kind},${order},${subscribers},${median(perOp).toFixed(1)},${median(slowest).toFixed(0)}`,
			);
		}
	} finally {
		installed.close();
	}
}

if (import.meta.main) run();
