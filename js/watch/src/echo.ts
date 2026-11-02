import { Milli, Nano } from "@moq/clock";
import { Report, Echo as Schema } from "@moq/hang";
import * as Moq from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys } from "@moq/signals";
import type { Broadcast } from "./broadcast";
import type { Received } from "./feedback";

/** A viewer report as written: per-rendition counters and the connection's round-trip time. */
type Snapshot = { transport?: { rtt: number }; renditions: Record<string, Received> };

/** Signals the reporter reads. */
export type EchoInput = {
	/** The origin to publish feedback into. */
	origin: Getter<Moq.Origin.Table | undefined>;
	/** Whether to publish. Defaults to true. */
	enabled: Getter<boolean>;
	/** The viewer's name, one path segment: feedback is published at `<echo path>/<name>.echo`. Undefined publishes none. */
	name: Getter<string | undefined>;
	/** The connection's PROBE estimates, whose round-trip time the report carries while someone reads it. */
	probe: Getter<Moq.Connection.Probe | undefined>;
};

/** What a decoder received of each rendition in a catalog, keyed by rendition ID. */
export interface Feedback {
	/** Report what was received of the renditions in the catalog of the broadcast at `path`. */
	feedback(path: Moq.Path.Valid): Record<string, Received>;
}

/** Constructor properties for {@link Echo}. */
export type EchoProps = Inputs<EchoInput> & {
	/** The watched broadcast, whose catalog may solicit feedback. */
	broadcast: Broadcast;
	/** The decoders whose counters the report carries. */
	sources: Feedback[];
};

/**
 * Refuse an echo name that is not one visible path segment: empty, containing `/`, a dot segment
 * or glob, or starting with `.`, which would hide the feedback from discovery. Matches the Rust CLI.
 */
export function checkName(name: string): string {
	if (name === "" || name.includes("/") || name === "." || name === ".." || name === "*" || name === "**") {
		throw new Error(`echo name ${JSON.stringify(name)} is not a single path segment`);
	}
	if (name.startsWith(".")) {
		throw new Error(`echo name ${JSON.stringify(name)} starts with \`.\`, which hides its feedback from discovery`);
	}
	return name;
}

/**
 * Publishes a viewer's feedback where a watched catalog solicits it.
 *
 * Once the catalog carries an `echo` section, this resolves its path against the broadcast, as a
 * rendition's `broadcast` reference resolves, and publishes `<name>.echo` there with one
 * `feedback.json` snapshot track: what each rendition was received and played, keyed by its ID in
 * that catalog, and the connection's PROBE round-trip time. Each catalog update reconciles it: a
 * removed section, or a changed path, retracts the broadcast it replaces.
 *
 * Reporters on one origin that resolve the same feedback path for the same catalog share one
 * broadcast and add their counts, since a second broadcast at a path would replace the first. A
 * feedback broadcast belongs to one soliciting catalog, so a reporter for another catalog that
 * resolves a path already in use publishes nothing and warns: give it another name.
 */
export class Echo {
	/** The wired inputs, read-only. */
	readonly in: Readonlys<EchoInput>;
	/** The watched broadcast. */
	readonly broadcast: Broadcast;

	readonly #sources: Feedback[];
	#signals = new Effect();

	constructor(props: EchoProps) {
		this.in = {
			origin: getter(props.origin),
			enabled: getter(props.enabled ?? true),
			name: getter(props.name),
			probe: getter(props.probe),
		};
		this.broadcast = props.broadcast;
		this.#sources = props.sources;

		// Derived on its own, so an ordinary catalog update that keeps the echo path keeps the broadcast.
		const target = this.#signals.computed((effect) => {
			if (!effect.get(this.in.enabled)) return undefined;
			const origin = effect.get(this.in.origin);
			const name = effect.get(this.in.name);
			const echo = effect.get(this.broadcast.out.catalog)?.echo;
			if (!origin || name === undefined || !echo) return undefined;

			const base = effect.get(this.broadcast.in.name);
			// The catalog consumer already refuses a path above the root; this only narrows the type.
			const prefix = Moq.Path.tryResolve(base, echo.path);
			if (prefix === undefined) return undefined;
			return { origin, base, path: Moq.Path.join(prefix, Moq.Path.from(`${checkName(name)}${Schema.SUFFIX}`)) };
		});

		this.#signals.run((effect) => {
			const current = effect.get(target);
			if (!current) return;

			let shared: Shared;
			try {
				shared = Shared.acquire(current.origin, current.path, current.base);
			} catch (err) {
				console.warn("failed to publish feedback", current.path, err);
				return;
			}
			const leave = shared.join(this, () => this.#report(current.base));
			effect.cleanup(leave);

			// Observing the probe asks the peer for PROBE updates, so only do it while someone reads them.
			effect.run((inner) => {
				if (inner.get(shared.watched)) inner.get(this.in.probe);
			});
		});
	}

	// What the decoders received of the renditions in the catalog of the broadcast at `base`.
	#report(base: Moq.Path.Valid): Snapshot {
		const renditions: Record<string, Received> = {};
		for (const source of this.#sources) Object.assign(renditions, source.feedback(base));
		const rtt = this.in.probe.peek()?.rtt;
		return rtt === undefined ? { renditions } : { transport: { rtt: Math.floor(rtt) }, renditions };
	}

	/** Stop publishing and release everything this holds. */
	close(): void {
		this.#signals.close();
	}
}

// One feedback broadcast per origin and path, shared by every reporter that resolves to it.
const published = new WeakMap<Moq.Origin.Table, Map<Moq.Path.Valid, Shared>>();

class Shared {
	/** The broadcast whose catalog solicited this feedback, which it belongs to. */
	readonly base: Moq.Path.Valid;

	readonly #origin: Moq.Origin.Table;
	readonly #path: Moq.Path.Valid;
	readonly #broadcast: Moq.Broadcast.Producer;
	readonly #writer: Report.Writer<Schema.Snapshot>;
	// Each reporter's counts, keyed by its owner.
	readonly #reporters = new Map<object, () => Snapshot>();
	// The final counts of the owners that left, summed, so the counters stay cumulative without them.
	#departed: Snapshot = { renditions: {} };
	// Each departed owner's share of #departed. Its counts are cumulative, so a returning owner takes
	// its share back out and reports it again itself. Weak, so a departed owner is not retained.
	readonly #shares = new WeakMap<object, Snapshot>();
	#signals = new Effect();

	/** Whether anyone reads the feedback, which is when it is written. */
	readonly watched: Getter<boolean | undefined>;

	/**
	 * The feedback broadcast at `path` for the catalog of the broadcast at `base`. Throws when it
	 * already reports for another catalog, since a second broadcast at the path would replace it.
	 */
	static acquire(origin: Moq.Origin.Table, path: Moq.Path.Valid, base: Moq.Path.Valid): Shared {
		let paths = published.get(origin);
		if (!paths) {
			paths = new Map();
			published.set(origin, paths);
		}
		let shared = paths.get(path);
		if (shared && shared.base !== base) {
			if (shared.#reporters.size > 0) {
				throw new Error(`${path} already reports feedback for the catalog of ${shared.base}`);
			}
			// Every reporter for the other catalog left, and its retraction is still pending: retract now.
			paths.delete(path);
			shared.#close();
			shared = undefined;
		}
		if (!shared) {
			shared = new Shared(origin, path, base);
			paths.set(path, shared);
		}
		return shared;
	}

	private constructor(origin: Moq.Origin.Table, path: Moq.Path.Valid, base: Moq.Path.Valid) {
		this.#origin = origin;
		this.#path = path;
		this.base = base;
		const publication = origin.createBroadcast(path);
		this.#broadcast = publication.broadcast;
		try {
			this.#writer = new Report.Writer(this.#broadcast, Schema.TRACK, Schema.SnapshotSchema);
			// A fresh instance, so a reader sees a replaced feedback broadcast as a restart.
			publication.announce({ epoch: Moq.Epoch.mint() });
		} catch (err) {
			this.#broadcast.close();
			throw err;
		}
		const demand = this.#writer.track.demand();
		this.watched = this.#signals.computed((effect) => effect.get(demand.wanted) !== undefined);
		this.#signals.interval(() => this.#write(), Nano.fromMilli(Milli(Report.INTERVAL)));
	}

	/** Add `owner`'s counts to the feedback, returning the function that removes them. */
	join(owner: object, reporter: () => Snapshot): () => void {
		const share = this.#shares.get(owner);
		if (share) {
			this.#shares.delete(owner);
			this.#departed = subtract(this.#departed, share);
		}
		this.#reporters.set(owner, reporter);
		return () => {
			if (this.#reporters.get(owner) !== reporter) return;
			this.#reporters.delete(owner);
			const final: Snapshot = { renditions: reporter().renditions };
			this.#shares.set(owner, final);
			this.#departed = merge(this.#departed, final);
			// Deferred, so a reporter's rerun reuses the broadcast rather than replacing it.
			queueMicrotask(() => {
				if (this.#reporters.size > 0 || published.get(this.#origin)?.get(this.#path) !== this) return;
				published.get(this.#origin)?.delete(this.#path);
				this.#close();
			});
		};
	}

	#write(): void {
		if (!this.#writer.wanted) return;
		let snapshot = this.#departed;
		for (const reporter of this.#reporters.values()) snapshot = merge(snapshot, reporter());
		this.#writer.update(snapshot as Schema.Snapshot);
	}

	// Write the final counts and retract the broadcast.
	#close(): void {
		this.#write();
		this.#signals.close();
		this.#writer.finish();
		this.#broadcast.close();
	}
}

// Add two reports' counters per rendition, keeping the newer arrival and the first round-trip time:
// gauges are never summed.
function merge(a: Snapshot, b: Snapshot): Snapshot {
	const renditions: Record<string, Received> = { ...a.renditions };
	for (const [id, received] of Object.entries(b.renditions)) {
		const sum: Received = { ...renditions[id] };
		for (const [counter, n] of Object.entries(received)) {
			if (counter === "arrival") continue;
			const key = counter as Exclude<keyof Received, "arrival">;
			sum[key] = (sum[key] ?? 0) + (n as number);
		}
		const arrival = received.arrival;
		if (arrival && (sum.arrival === undefined || arrival.wall >= sum.arrival.wall)) sum.arrival = arrival;
		renditions[id] = sum;
	}
	const transport = a.transport ?? b.transport;
	return transport ? { transport, renditions } : { renditions };
}

// Take a departed reporter's counters back out of `a`. The newest arrival stays, since it is a gauge.
function subtract(a: Snapshot, b: Snapshot): Snapshot {
	const renditions: Record<string, Received> = { ...a.renditions };
	for (const [id, received] of Object.entries(b.renditions)) {
		const rest: Received = { ...renditions[id] };
		for (const [counter, n] of Object.entries(received)) {
			if (counter === "arrival") continue;
			const key = counter as Exclude<keyof Received, "arrival">;
			const left = (rest[key] ?? 0) - (n as number);
			if (left > 0) rest[key] = left;
			else delete rest[key];
		}
		renditions[id] = rest;
	}
	return { ...a, renditions };
}
