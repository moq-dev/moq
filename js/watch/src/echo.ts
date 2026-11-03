import { Milli, Nano } from "@moq/clock";
import { Report, Echo as Schema } from "@moq/hang";
import * as Moq from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, Signal } from "@moq/signals";
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
 * A `\` must escape another `\` in the published name, as in any path.
 */
export function checkName(name: string): string {
	if (name === "" || name.includes("/") || name === "." || name === ".." || name === "*" || name === "**") {
		throw new Error(`echo name ${JSON.stringify(name)} is not a single path segment`);
	}
	if (name.startsWith(".")) {
		throw new Error(`echo name ${JSON.stringify(name)} starts with \`.\`, which hides its feedback from discovery`);
	}
	try {
		Moq.Path.from(`${name}${Schema.SUFFIX}`);
	} catch (err) {
		throw new Error(`echo name ${JSON.stringify(name)} is not a valid path segment`, { cause: err });
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
 * Each reporter publishes its own broadcast, so a report is only ever its own decoders' counts. A
 * second broadcast at a path would replace the first, so a reporter that resolves a feedback path
 * another reporter on the same origin already publishes publishes nothing and warns until that one
 * releases it: give it another name.
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
			const { origin, path, base } = current;

			let claims = claimed.get(origin);
			if (!claims) {
				claims = { paths: new Set(), released: new Signal(0) };
				claimed.set(origin, claims);
			}
			if (claims.paths.has(path)) {
				// Try again whenever a reporter on this origin releases its path.
				effect.get(claims.released);
				console.warn(
					"feedback path already in use on this connection; give this player another echo name",
					path,
				);
				return;
			}

			let broadcast: Moq.Broadcast.Producer | undefined;
			let writer: Report.Writer<Schema.Snapshot>;
			try {
				const publication = origin.createBroadcast(path);
				broadcast = publication.broadcast;
				writer = new Report.Writer(broadcast, Schema.TRACK, Schema.SnapshotSchema);
				// A fresh instance, so a reader sees a replaced feedback broadcast as a restart.
				publication.announce();
			} catch (err) {
				broadcast?.close();
				console.warn("failed to publish feedback", path, err);
				return;
			}
			const published = broadcast;

			const claim = claims;
			claim.paths.add(path);
			const write = () => writer.update(this.#report(base) as Schema.Snapshot);
			// Teardown runs last in, first out: write the final counts, then retract and release the path.
			effect.cleanup(() => {
				claim.paths.delete(path);
				claim.released.update((n) => n + 1);
			});
			effect.cleanup(() => published.close());
			effect.cleanup(() => writer.finish());
			effect.cleanup(write);
			effect.interval(write, Nano.fromMilli(Milli(Report.INTERVAL)));

			// Observing the probe asks the peer for PROBE updates, so only do it while someone reads them.
			const demand = writer.track.demand();
			effect.run((inner) => {
				if (inner.get(demand.wanted) !== undefined) inner.get(this.in.probe);
			});
		});
	}

	// What the decoders received of the renditions in the catalog of the broadcast at `base`.
	#report(base: Moq.Path.Valid): Snapshot {
		// Not Object.assign, which sets the prototype for an ID like `__proto__` instead of copying it.
		const renditions: Record<string, Received> = Object.fromEntries(
			this.#sources.flatMap((source) => Object.entries(source.feedback(base))),
		);
		const rtt = this.in.probe.peek()?.rtt;
		return rtt === undefined ? { renditions } : { transport: { rtt: Math.floor(rtt) }, renditions };
	}

	/** Stop publishing and release everything this holds. */
	close(): void {
		this.#signals.close();
	}
}

// The feedback paths published on each origin, so a second reporter never replaces the first's
// broadcast, and a count of releases, so a refused reporter tries again once one frees.
const claimed = new WeakMap<Moq.Origin.Table, { paths: Set<Moq.Path.Valid>; released: Signal<number> }>();
