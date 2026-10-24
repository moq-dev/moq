/**
 * Client health from two consecutive media reports.
 * Counter deltas over a stated interval, graded against thresholds; container sections such as
 * `mpegts` are never read. A client with no report has no sample: its verdict is `unknown`.
 * @module
 */

import type { Epoch, Path } from "@moq/net";
import type * as Echo from "./echo";
import type * as Stats from "./stats";

/** A grade, ordered by severity with `unknown` below every graded one. */
export type Verdict = "unknown" | "healthy" | "degraded" | "unhealthy";

/** Which client wrote a report, and so which of its numbers can be graded. */
export type Role = "publisher" | "viewer";

/** The broadcast a report was read from, which observed every number in it. */
export interface Observer {
	/** The publisher's media broadcast, or the viewer's echo broadcast. */
	path: Path.Valid;
	/** The reporting broadcast's publisher epoch, when its route carries one. */
	epoch?: Epoch.Valid;
}

/** One snapshot and the broadcast it was read from. */
export interface Report<S> {
	/** Who observed the snapshot's numbers. */
	observer: Observer;
	/** The cumulative snapshot. */
	snapshot: S;
}

/** Two consecutive reports from one client and the time between them. */
export interface Sample<S> {
	/** The report before `current`, absent for the first, which has nothing to diff against. */
	previous?: Report<S>;
	/** The newest report. */
	current: Report<S>;
	/** Milliseconds between the two reports, over which counter deltas become rates. */
	interval: number;
	/** Milliseconds since `current` was reported. Past `stale` every verdict is unknown. */
	age: number;
}

/** The values at which an input grades degraded and unhealthy (below them, for `headroom`). */
export interface Limits {
	/** Degraded from this value on. */
	degraded: number;
	/** Unhealthy from this value on. */
	unhealthy: number;
}

/** Grading limits, nested by the report section they apply to. */
export interface Thresholds {
	/**
	 * Milliseconds past which a report grades unknown, as does a transport sample by its age plus the report's.
	 * Loss also needs the previous transport sample's age within it.
	 */
	stale: number;
	/** Limits for the reporting connection: RTT in milliseconds, then loss and headroom (publisher only). */
	transport: { rtt: Limits; loss: Limits; headroom: Limits };
	/** Limits for a publisher's renditions. */
	publisher: { skipped: Limits };
	/** Limits for a viewer's renditions. */
	viewer: { late: Limits; errors: Limits; stalled: Limits };
}

/**
 * A fresh copy of the documented default thresholds; override a field to change one.
 * The transport limits are measured on a paced TS feed through a shaped link, as `hang::health::Thresholds` records.
 */
export function defaultThresholds(): Thresholds {
	const share = () => ({ degraded: 0.01, unhealthy: 0.05 });
	return {
		stale: 5000,
		transport: {
			rtt: { degraded: 300, unhealthy: 800 },
			loss: { degraded: 0.02, unhealthy: 0.1 },
			headroom: { degraded: 1.5, unhealthy: 1.0 },
		},
		publisher: { skipped: share() },
		viewer: { late: share(), errors: share(), stalled: share() },
	};
}

/** Why a sample's deltas started over: a counter went down, or the broadcast was replaced. */
export type Reset = "decrease" | "replaced";

/** An input a verdict can rest on: RTT in milliseconds; the rest are ratios over the interval. */
export type Input = "rtt" | "loss" | "headroom" | "skipped" | "late" | "errors" | "stalled";

/** One input's value and the grade it earned. */
export interface Evidence {
	/** The input graded. */
	input: Input;
	/** The value in the input's unit, absent when the reports cannot show it. */
	value?: number;
	/** The value's grade: unknown when the value is absent or stale. */
	verdict: Verdict;
}

/** The reporting connection's grade and its loss over the interval. */
export interface Connection {
	/** The worst graded evidence, or unknown when none was graded. */
	verdict: Verdict;
	/** Every input graded for the role, in a fixed order. */
	evidence: Evidence[];
	/** The transport sample plus the report's age exceeds `stale`, so its evidence grades unknown. */
	stale?: true;
	/** Bytes lost over the interval. */
	lostBytes?: number;
	/** Packets lost over the interval. */
	lostPackets?: number;
}

/** One rendition's grade and its delivery over the interval. */
export interface Rendition {
	/** The worst graded evidence, or unknown when none was graded. */
	verdict: Verdict;
	/** Every input graded for the role, in a fixed order. */
	evidence: Evidence[];
	/** Frames sent or received over the interval. */
	frames?: number;
	/** Media payload bytes sent or received over the interval. */
	bytes?: number;
	/** Media bitrate over the interval, in bits per second. */
	bitrate?: number;
}

/** A client's health over one sample, every number observed by `observer`. */
export interface Health {
	/** Which client wrote the reports. */
	role: Role;
	/** The broadcast the current report was read from. */
	observer: Observer;
	/** The worst verdict of the connection and every rendition. */
	verdict: Verdict;
	/** The report is older than `stale`, so every verdict is unknown. */
	stale?: true;
	/** Milliseconds the deltas span; absent without a previous report to diff against. */
	interval?: number;
	/** Why the deltas started over instead of spanning the interval. */
	reset?: Reset;
	/** The reporting connection. */
	transport: Connection;
	/** Each rendition in the current report, keyed by its catalog ID. */
	renditions: Record<string, Rendition>;
}

/**
 * Grade a publisher's self-report.
 * Loss and headroom divide the connection's transport counters by this broadcast's media, so they
 * hold when the broadcast is the only one its connection publishes.
 */
export function publisher(sample: Sample<Stats.Snapshot>, thresholds: Thresholds = defaultThresholds()): Health {
	return assess(sample, PUBLISHER, thresholds);
}

/** Grade a viewer's feedback. */
export function viewer(sample: Sample<Echo.Snapshot>, thresholds: Thresholds = defaultThresholds()): Health {
	return assess(sample, VIEWER, thresholds);
}

interface Snapshot<R> {
	transport?: Stats.Transport;
	renditions?: Record<string, R>;
}

interface Delta<R> {
	previous: R;
	current: R;
	interval: number;
}

interface Kind<R> {
	role: Role;
	frames(rendition: R): number;
	bytes(rendition: R): number;
	decreased(previous: R, current: R): boolean;
	evidence(delta: Delta<R> | undefined, thresholds: Thresholds): Evidence[];
}

const SEVERITY: Record<Verdict, number> = { unknown: 0, healthy: 1, degraded: 2, unhealthy: 3 };

function count<R>(delta: Delta<R>, counter: (rendition: R) => number | undefined): number {
	return Math.max(0, (counter(delta.current) ?? 0) - (counter(delta.previous) ?? 0));
}

function lower<R>(previous: R, current: R, counters: ((rendition: R) => number | undefined)[]): boolean {
	return counters.some((counter) => (counter(current) ?? 0) < (counter(previous) ?? 0));
}

const PUBLISHER: Kind<Stats.Rendition> = {
	role: "publisher",
	frames: (r) => r.frames ?? 0,
	bytes: (r) => r.bytes ?? 0,
	decreased: (previous, current) =>
		lower(previous, current, [(r) => r.frames, (r) => r.bytes, (r) => r.keyframes, (r) => r.skipped]),
	evidence: (delta, thresholds) => {
		let share: number | undefined;
		if (delta) {
			const skipped = count(delta, (r) => r.skipped);
			share = ratio(skipped, count(delta, (r) => r.frames) + skipped);
		}
		return [above("skipped", share, thresholds.publisher.skipped)];
	},
};

const VIEWER: Kind<Echo.Rendition> = {
	role: "viewer",
	frames: (r) => r.frames ?? 0,
	bytes: (r) => r.bytes ?? 0,
	decreased: (previous, current) =>
		lower(previous, current, [
			(r) => r.frames,
			(r) => r.bytes,
			(r) => r.decoded,
			(r) => r.late,
			(r) => r.errors,
			(r) => r.stalls,
			(r) => r.stalled,
			(r) => r.underruns,
		]),
	evidence: (delta, thresholds) => {
		const limits = thresholds.viewer;
		const perFrame = (counter: (r: Echo.Rendition) => number | undefined) =>
			delta &&
			ratio(
				count(delta, counter),
				count(delta, (r) => r.frames),
			);
		const stalled = delta && count(delta, (r) => r.stalled) / delta.interval;
		return [
			above(
				"late",
				perFrame((r) => r.late),
				limits.late,
			),
			above(
				"errors",
				perFrame((r) => r.errors),
				limits.errors,
			),
			above("stalled", stalled, limits.stalled),
		];
	},
};

function assess<R>(sample: Sample<Snapshot<R>>, kind: Kind<R>, thresholds: Thresholds): Health {
	const valid = (ms: number) => Number.isFinite(ms) && ms >= 0;
	if (!valid(sample.interval) || !valid(sample.age)) {
		throw new Error(
			`health: interval ${sample.interval} and age ${sample.age} must be finite nonnegative milliseconds`,
		);
	}
	const current = sample.current.snapshot;
	let previous: Snapshot<R> | undefined;
	let reset: Reset | undefined;
	if (sample.previous) {
		if (!sameObserver(sample.previous.observer, sample.current.observer)) reset = "replaced";
		else if (decreased(sample.previous.snapshot, current, kind)) reset = "decrease";
		else if (sample.interval > 0) previous = sample.previous.snapshot;
	}
	const interval = previous ? sample.interval : undefined;

	let sent = 0;
	const entries: [string, Rendition][] = [];
	for (const [id, rendition] of Object.entries(current.renditions ?? {})) {
		const delta = previous && {
			previous: own(previous.renditions, id) ?? ({} as R),
			current: rendition,
			interval: sample.interval,
		};
		const evidence = kind.evidence(delta, thresholds);
		const entry: Rendition = { verdict: worst(evidence), evidence };
		if (delta && interval !== undefined) {
			const bytes = count(delta, kind.bytes);
			sent += bytes;
			entry.frames = count(delta, kind.frames);
			entry.bytes = bytes;
			entry.bitrate = bitrate(bytes, interval);
		}
		entries.push([id, entry]);
	}
	// fromEntries defines own properties, so an ID such as `__proto__` stays a key.
	const renditions: Record<string, Rendition> = Object.fromEntries(entries);

	const transport = connection(
		{
			current: current.transport ?? {},
			previous: previous && (previous.transport ?? {}),
			age: sample.age,
			sent,
			media: interval === undefined ? undefined : bitrate(sent, interval),
		},
		kind.role,
		thresholds,
	);
	const health: Health = {
		role: kind.role,
		observer: sample.current.observer,
		verdict: "unknown",
		transport,
		renditions,
	};
	if (interval !== undefined) health.interval = interval;
	if (reset) health.reset = reset;
	if (sample.age > thresholds.stale) {
		health.stale = true;
		forget(transport);
		for (const rendition of Object.values(renditions)) forget(rendition);
	}
	health.verdict = Object.values(renditions).reduce<Verdict>(
		(verdict, rendition) => max(verdict, rendition.verdict),
		transport.verdict,
	);
	return health;
}

interface Link {
	current: Stats.Transport;
	previous?: Stats.Transport;
	age: number;
	sent: number;
	media?: number;
}

function connection(link: Link, role: Role, thresholds: Thresholds): Connection {
	const limits = thresholds.transport;
	const fresh = link.previous && !((link.previous.age ?? 0) > thresholds.stale) ? link.previous : undefined;
	const lost = (counter: (t: Stats.Transport) => number | undefined) => {
		const current = counter(link.current);
		const previous = fresh && counter(fresh);
		return current === undefined || previous === undefined ? undefined : Math.max(0, current - previous);
	};
	const lostBytes = lost((t) => t.lostBytes);
	const lostPackets = lost((t) => t.lostPackets);

	const evidence = [above("rtt", link.current.rtt, limits.rtt)];
	if (role === "publisher") {
		const loss = lostBytes === undefined ? undefined : ratio(lostBytes, link.sent);
		const rate = link.current.rate;
		const headroom = rate !== undefined && link.media ? rate / link.media : undefined;
		evidence.push(above("loss", loss, limits.loss));
		evidence.push(below("headroom", headroom, limits.headroom));
	}

	const result: Connection = { verdict: "unknown", evidence };
	const age = link.current.age;
	if (age !== undefined && age + link.age > thresholds.stale) {
		result.stale = true;
		for (const e of evidence) e.verdict = "unknown";
	}
	result.verdict = worst(evidence);
	if (lostBytes !== undefined) result.lostBytes = lostBytes;
	if (lostPackets !== undefined) result.lostPackets = lostPackets;
	return result;
}

function sameObserver(a: Observer, b: Observer): boolean {
	return a.path === b.path && a.epoch === b.epoch;
}

function decreased<R>(previous: Snapshot<R>, current: Snapshot<R>, kind: Kind<R>): boolean {
	const lost = (counter: (t: Stats.Transport) => number | undefined) => {
		const before = previous.transport && counter(previous.transport);
		const after = current.transport && counter(current.transport);
		return before !== undefined && after !== undefined && after < before;
	};
	if (lost((t) => t.lostBytes) || lost((t) => t.lostPackets)) return true;
	return Object.entries(current.renditions ?? {}).some(([id, rendition]) => {
		const before = own(previous.renditions, id);
		return before !== undefined && kind.decreased(before, rendition);
	});
}

function own<R>(renditions: Record<string, R> | undefined, id: string): R | undefined {
	return renditions && Object.hasOwn(renditions, id) ? renditions[id] : undefined;
}

function ratio(count: number, of: number): number | undefined {
	return of > 0 ? count / of : undefined;
}

// Rust saturates a bitrate at u64::MAX, which is 2 ** 64 as a double.
const U64_MAX = 2 ** 64;

function bitrate(bytes: number, interval: number): number {
	return Math.min(Math.floor((bytes * 8000) / interval), U64_MAX);
}

function grade(input: Input, value: number | undefined, verdict: Verdict): Evidence {
	return value === undefined ? { input, verdict } : { input, value, verdict };
}

function above(input: Input, value: number | undefined, limits: Limits): Evidence {
	if (value === undefined) return grade(input, value, "unknown");
	if (value >= limits.unhealthy) return grade(input, value, "unhealthy");
	if (value >= limits.degraded) return grade(input, value, "degraded");
	return grade(input, value, "healthy");
}

function below(input: Input, value: number | undefined, limits: Limits): Evidence {
	if (value === undefined) return grade(input, value, "unknown");
	if (value < limits.unhealthy) return grade(input, value, "unhealthy");
	if (value < limits.degraded) return grade(input, value, "degraded");
	return grade(input, value, "healthy");
}

function max(a: Verdict, b: Verdict): Verdict {
	return SEVERITY[b] > SEVERITY[a] ? b : a;
}

function worst(evidence: Evidence[]): Verdict {
	return evidence.reduce<Verdict>((verdict, e) => max(verdict, e.verdict), "unknown");
}

function forget(grade: { verdict: Verdict; evidence: Evidence[] }) {
	grade.verdict = "unknown";
	for (const e of grade.evidence) e.verdict = "unknown";
}
