/**
 * Client health from two consecutive media reports.
 * Counter deltas over a stated interval, graded against thresholds; container sections such as
 * `mpegts` are never read. A client with no report has no sample: its verdict is `unknown`.
 * @module
 */

import { Duration } from "@moq/clock";
import type { Identity, Path } from "@moq/net";
import type * as Echo from "./echo.ts";
import type * as Stats from "./stats.ts";

/** A grade, ordered by severity with `unknown` below every graded one. */
export type Verdict = "unknown" | "healthy" | "degraded" | "unhealthy";

/** Which client wrote a report, and so which of its numbers can be graded. */
export type Role = "publisher" | "viewer";

/** The broadcast a report was read from, which observed every number in it. */
export interface Observer {
	/** The publisher's media broadcast, or the viewer's echo broadcast. */
	path: Path.Valid;
	/** The identity of the reporting broadcast's stream, when its route carries one. */
	identity?: Identity.Valid;
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
	/** The time between the two reports, over which counter deltas become rates. */
	interval: Duration;
	/** How long ago `current` was reported. Past `stale` every verdict is unknown. */
	age: Duration;
}

/**
 * Diff `sample.current` against `previous`, over the time between the two reports' `transport.uptime`s.
 * A report without one, or an uptime that did not advance, leaves the sample with nothing to diff
 * against, dropping any `previous` it held: arrival times say nothing about the time between the samples, since a congested report arrives
 * late and in a burst.
 */
export function withUptime<S extends { transport?: Stats.Transport }>(
	sample: Sample<S>,
	previous: Report<S>,
): Sample<S> {
	const before = previous.snapshot.transport?.uptime;
	const after = sample.current.snapshot.transport?.uptime;
	if (before === undefined || after === undefined || after <= before) {
		return { ...sample, previous: undefined, interval: Duration.ZERO };
	}
	return { ...sample, previous, interval: Duration.millis(after - before) };
}

/** The values at which an input grades degraded and unhealthy (below them, for `headroom`). */
export interface Limits<T = number> {
	/** Degraded from this value on. */
	degraded: T;
	/** Unhealthy from this value on. */
	unhealthy: T;
}

/** Grading limits, nested by the report section they apply to. */
export interface Thresholds {
	/**
	 * A report older than this grades unknown, as does a transport sample whose age plus the report's exceeds it.
	 * Loss also needs the previous transport sample's age within it.
	 */
	stale: Duration;
	/** Limits for the reporting connection. */
	transport: TransportThresholds;
	/** Limits for a publisher's renditions. */
	publisher: PublisherThresholds;
	/** Limits for a viewer's renditions. */
	viewer: ViewerThresholds;
}

/** Limits for the reporting connection. */
export interface TransportThresholds {
	/** Round-trip time. */
	rtt: Limits<Duration>;
	/** Transport bytes lost per byte the connection sent (publisher only). */
	loss: Limits;
	/** Estimated rate over the bitrate the connection sent (publisher only); lower is worse. */
	headroom: Limits;
}

/** Limits for a publisher's renditions. */
export interface PublisherThresholds {
	/** Frames skipped before sending, as a share of frames sent or skipped. */
	skipped: Limits;
}

/** Limits for a viewer's renditions. */
export interface ViewerThresholds {
	/** Frames received too late for playout, per frame received. */
	late: Limits;
	/** Decode errors per frame received. */
	errors: Limits;
	/** The share of the interval spent stalled. */
	stalled: Limits;
}

/**
 * A fresh copy of the documented default thresholds; override a field to change one.
 * The transport limits are measured on a paced TS feed through a shaped link, as `hang::health::Thresholds` records.
 */
export function defaultThresholds(): Thresholds {
	const share = () => ({ degraded: 0.01, unhealthy: 0.05 });
	return {
		stale: Duration.secs(5),
		transport: {
			rtt: { degraded: Duration.millis(300), unhealthy: Duration.millis(800) },
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
	/** Milliseconds the deltas span, as Rust serializes it; absent without a previous report to diff against. */
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
 * Loss and headroom divide the connection's loss and rate by the bytes the connection sent, so they
 * hold however many broadcasts it carries. A report without `sentBytes` grades them unknown.
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
	interval: Duration;
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
		const stalled = delta && count(delta, (r) => r.stalled) / delta.interval.asMillis();
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
	const current = sample.current.snapshot;
	let previous: Snapshot<R> | undefined;
	let reset: Reset | undefined;
	if (sample.previous) {
		if (!sameObserver(sample.previous.observer, sample.current.observer)) reset = "replaced";
		else if (decreased(sample.previous.snapshot, current, kind)) reset = "decrease";
		else if (!sample.interval.isZero()) previous = sample.previous.snapshot;
	}
	const interval = previous ? sample.interval : undefined;

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
			interval,
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
	// Whole milliseconds, as Rust truncates the interval it serializes.
	if (interval !== undefined) health.interval = Math.floor(interval.asMillis());
	if (reset) health.reset = reset;
	if (sample.age.gt(thresholds.stale)) {
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
	age: Duration;
	interval?: Duration;
}

function connection(link: Link, role: Role, thresholds: Thresholds): Connection {
	const limits = thresholds.transport;
	const previous = link.previous;
	const fresh =
		previous && (previous.age === undefined || Duration.millis(previous.age).le(thresholds.stale))
			? previous
			: undefined;
	const delta = (counter: (t: Stats.Transport) => number | undefined) => {
		const current = counter(link.current);
		const previous = fresh && counter(fresh);
		return current === undefined || previous === undefined ? undefined : Math.max(0, current - previous);
	};
	const lostBytes = delta((t) => t.lostBytes);
	const lostPackets = delta((t) => t.lostPackets);
	const sent = delta((t) => t.sentBytes);

	const rtt = { degraded: limits.rtt.degraded.asMillis(), unhealthy: limits.rtt.unhealthy.asMillis() };
	const evidence = [above("rtt", link.current.rtt, rtt)];
	if (role === "publisher") {
		const loss = lostBytes === undefined || sent === undefined ? undefined : ratio(lostBytes, sent);
		// The counters were sampled `age` before each report, so they span the interval between the
		// reports, widened by the earlier sample's age and narrowed by the later's.
		const span =
			fresh && link.interval
				? link.interval
						.saturatingAdd(Duration.millis(fresh.age ?? 0))
						.saturatingSub(Duration.millis(link.current.age ?? 0))
				: undefined;
		const media = sent === undefined || !span || span.isZero() ? undefined : bitrate(sent, span);
		const rate = link.current.rate;
		const headroom = rate !== undefined && media ? rate / media : undefined;
		evidence.push(above("loss", loss, limits.loss));
		evidence.push(below("headroom", headroom, limits.headroom));
	}

	const result: Connection = { verdict: "unknown", evidence };
	const age = link.current.age;
	if (age !== undefined && Duration.millis(age).saturatingAdd(link.age).gt(thresholds.stale)) {
		result.stale = true;
		for (const e of evidence) e.verdict = "unknown";
	}
	result.verdict = worst(evidence);
	if (lostBytes !== undefined) result.lostBytes = lostBytes;
	if (lostPackets !== undefined) result.lostPackets = lostPackets;
	return result;
}

function sameObserver(a: Observer, b: Observer): boolean {
	return a.path === b.path && a.identity === b.identity;
}

function decreased<R>(previous: Snapshot<R>, current: Snapshot<R>, kind: Kind<R>): boolean {
	const wentDown = (counter: (t: Stats.Transport) => number | undefined) => {
		const before = previous.transport && counter(previous.transport);
		const after = current.transport && counter(current.transport);
		return before !== undefined && after !== undefined && after < before;
	};
	if (wentDown((t) => t.lostBytes) || wentDown((t) => t.lostPackets) || wentDown((t) => t.sentBytes)) return true;
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

function bitrate(bytes: number, interval: Duration): number {
	return Math.min(Math.floor((bytes * 8000) / interval.asMillis()), U64_MAX);
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
