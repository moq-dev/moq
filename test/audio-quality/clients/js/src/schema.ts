/**
 * The metric contract: what the page samples, what a row reduces to, and what a budget grades.
 *
 * This file is the schema, not a description of one. The page emits {@link Sample}s, `analyze.ts`
 * reduces them to a {@link Summary}, and `grade.ts` reads that against `budgets.json`. {@link METRICS}
 * says for every graded number what it counts, which clock it sits on, and how a series became one
 * value. Another lane (native playout, the latency ledger) that emits the same {@link Summary} is
 * comparable to this one by construction.
 *
 * Conventions, stated once for everything below:
 *
 * - Every duration is milliseconds as a float. A sample count is converted at the rate it was
 *   counted at before it reaches a {@link Summary}.
 * - Every count is a non-negative integer. Every share is a fraction of 1.
 * - Every timestamp sits on a named {@link Clock} and is reduced to `viewer` before any two are
 *   subtracted. {@link Drift} records what each reduction took.
 *
 * @module
 */

/** Milliseconds, as a float. The unit of every duration in this schema. */
export type Ms = number;

/**
 * The clocks a timestamp can be taken on.
 *
 * - `viewer`: the page's `performance.now()`, monotonic. The reference every other clock is reduced
 *   to, so a stage span is always a difference of two `viewer` values.
 * - `render`: the page's `AudioContext` frame clock, the audio device's oscillator. Same host, so it
 *   is reduced by an offset and a drift fitted over the run from paired readings (see
 *   {@link MAX_RENDER_DRIFT}).
 * - `media`: the publisher's presentation timestamps, seen through the stream. Not a clock the viewer
 *   can read directly: its rate against `viewer` is fitted, its offset is unknowable from here.
 * - `publisher`, `relay`: other processes, often other machines. A timestamp taken there is reduced
 *   to `viewer` by an offset measured over the same session, NTP style: the offset at the midpoint of
 *   a round trip, with half the round trip as its uncertainty. A stage whose uncertainty exceeds
 *   {@link identityTolerance} is reported unmeasured rather than guessed. The browser lane carries no
 *   such timestamp yet, so every stage that needs one is null here.
 */
export type Clock = "viewer" | "render" | "media" | "publisher" | "relay";

/**
 * How a series became the one number a budget grades.
 *
 * - `total`: summed over the graded window.
 * - `per_min`: `total` divided by the graded window's length in minutes.
 * - `p50`, `p95`: nearest-rank percentile of the window's samples.
 * - `max`: the largest value in the window.
 * - `share`: the fraction of the window for which the condition held.
 * - `last`: the value at the end of the window.
 */
export type Aggregation = "total" | "per_min" | "p50" | "p95" | "max" | "share" | "last";

/** What one metric is: enough to read a number off a summary without guessing. */
export type MetricSpec = {
	/** What the raw measurement is counted in, before aggregation. */
	unit: "ms" | "count" | "share";
	/** The clock its timestamps were taken on, before reduction to `viewer`. */
	clock: Clock;
	/** The aggregations reported for it, and therefore the budget keys that can grade it. */
	aggregations: Aggregation[];
	/** One plain line saying what it measures. */
	description: string;
};

/**
 * Every graded metric, keyed by its base name.
 *
 * A budget key is `<name>_<aggregation>`: `underruns` graded per minute is `underruns_per_min`. Naming
 * the aggregation in the key is what keeps "underruns: 3" and "underruns: 3/min" from being graded
 * against each other.
 */
export const METRICS = {
	underruns: {
		unit: "count",
		clock: "render",
		aggregations: ["total", "per_min"],
		description:
			"Render quanta the ring could only partly fill, or not fill at all, while it was playing. A quantum rendered while the ring is stalled is not one.",
	},
	short_quanta: {
		unit: "count",
		clock: "render",
		aggregations: ["total", "per_min"],
		description: "Of the underruns, the quanta the ring filled the front of and then ran dry part-way through.",
	},
	underrun_episodes: {
		unit: "count",
		clock: "render",
		aggregations: ["total", "per_min"],
		description: "Maximal runs of consecutive underruns: one audible gap each, however many quanta it spanned.",
	},
	underrun_ms: {
		unit: "ms",
		clock: "render",
		aggregations: ["total", "per_min", "max"],
		description:
			"Audio the ring failed to supply while playing: the missing samples at the device rate. `max` is the longest episode.",
	},
	skip_aheads: {
		unit: "count",
		clock: "viewer",
		aggregations: ["total", "per_min"],
		description:
			"Re-anchors that discarded buffered audio: a forward step in the lag between the viewer clock and the playhead.",
	},
	discarded_ms: {
		unit: "ms",
		clock: "media",
		aggregations: ["total", "per_min"],
		description:
			"Media time the skip-aheads jumped over without playing. Audio dropped before it reached the ring moves no playhead and is not counted.",
	},
	stalled: {
		unit: "share",
		clock: "viewer",
		aggregations: ["share"],
		description:
			"Share of the window the ring spent stalled, refilling rather than playing: silence the player chose.",
	},
	silence: {
		unit: "share",
		clock: "render",
		aggregations: ["share"],
		description:
			"Share of rendered quanta under the silence floor at the player's output, whatever the cause. The published tone is never quiet, so any of it is a gap or a stall.",
	},
	target_ms: {
		unit: "ms",
		clock: "viewer",
		aggregations: ["p50", "p95", "max"],
		description: "The playout delay the receiver resolved (`sync.out.delay`), sampled through the window.",
	},
	converge_ms: {
		unit: "ms",
		clock: "viewer",
		aggregations: ["last"],
		description:
			"From first audio to the last time the resolved target moved more than one bucket from its final value.",
	},
	render_load: {
		unit: "share",
		clock: "render",
		aggregations: ["max"],
		description:
			"Chromium's render capacity: the share of each quantum's budget the graph used. High says the runner, not the player, was short of time.",
	},
} as const satisfies Record<string, MetricSpec>;

/** One of the {@link METRICS} names. */
export type Metric = keyof typeof METRICS;

/** Every `<metric>_<aggregation>` key, in schema order, so tables read the same every run. */
export const METRIC_KEYS: string[] = Object.entries(METRICS).flatMap(([name, spec]) =>
	spec.aggregations.map((aggregation) => `${name}_${aggregation}`),
);

/** A quantum whose RMS at the player's output is under this is quiet. About -60 dBFS. */
export const SILENCE_RMS = 0.001;

/** How often the page samples. */
export const SAMPLE_INTERVAL_MS = 250;

/** A resolved target within this of its final value has settled. One estimator bucket. */
export const BUCKET_MS = 20;

/**
 * The render clock may run this far from the viewer's, as a fraction, before the row is void.
 *
 * Both oscillators are on one host, so real drift is parts per million. Past 1% the device is not
 * running at wall rate at all, which is a throttled or stalled context, and every render-clock count
 * would be read off a clock that was not moving.
 */
export const MAX_RENDER_DRIFT = 0.01;

/**
 * The stages an end-to-end delay splits into, in order, as exclusive spans.
 *
 * Exclusive is what makes the sum mean anything: each stage is defined by the two boundaries that
 * end the previous stage and start the next, so no two can claim the same millisecond.
 *
 * - `capture`: sound at the publisher's input to its samples being readable.
 * - `encode`: samples readable to the encoded frame existing.
 * - `publish_flush`: frame existing to it being written to the transport, where a publisher that
 *   batches frames holds them.
 * - `network`: written by the publisher to delivered to the viewer's container consumer, relay
 *   included.
 * - `jitter_buffer`: delivered to its decoded samples leaving the ring for a render quantum. The
 *   decoder runs inside this span in a live pipeline, so it is not also `decode`.
 * - `decode`: decode time that did not overlap the jitter buffer, which is zero while the ring holds
 *   anything.
 * - `render`: leaving the ring to leaving the audio graph.
 * - `device`: leaving the graph to the speaker (`outputLatency` plus `baseLatency`).
 * - `unaccounted`: end-to-end minus the sum of the rest. Named because a large one is the finding.
 *
 * The identity is `sum(stages) == end-to-end`, within {@link identityTolerance}.
 */
export const STAGES = [
	"capture",
	"encode",
	"publish_flush",
	"network",
	"jitter_buffer",
	"decode",
	"render",
	"device",
	"unaccounted",
] as const;

/** One of {@link STAGES}. */
export type Stage = (typeof STAGES)[number];

/** A stage span and where its number came from. */
export type StageSpan = {
	/** The span, or null when this lane cannot measure it. */
	ms: Ms | null;
	/** `measured` on the viewer clock, `declared` by the publisher, or `unmeasured`. */
	source: "measured" | "declared" | "unmeasured";
};

/** The tolerance on the sum-to-end-to-end identity: the larger of 2 ms or 2% of the total. */
export const identityTolerance = (endToEnd: Ms): Ms => Math.max(2, endToEnd * 0.02);

/** What reducing one clock to `viewer` took. */
export type Drift = {
	/** Which clock. */
	clock: Clock;
	/** Its rate against `viewer`, minus one: 0.001 runs 1 ms fast per second. Null when unmeasured. */
	rate: number | null;
	/** What was added to put it on the viewer epoch. Null when unmeasured or unknowable. */
	offset: Ms | null;
};

// ── the row ─────────────────────────────────────────────────────────────────

/** Which ring the page ran, decided by whether the document is cross-origin isolated. */
export type Ring = "isolated" | "plain";

/** The audio codecs the matrix publishes. */
export type Codec = "opus" | "aac";

/**
 * One matrix cell.
 *
 * A budget is keyed by the whole thing. The codec, its rate, and the ring each move the expected
 * floor as much as the profile does, so a profile-only key would grade one cell against another's
 * threshold.
 */
export type Row = {
	runtime: "chromium";
	codec: Codec;
	/** Sample rate in Hz. */
	rate: number;
	profile: string;
	ring: Ring;
};

/** The row as a flat string: a file name and a table label. */
export const rowKey = (row: Row): string => `${row.runtime}-${row.codec}-${row.rate}-${row.profile}-${row.ring}`;

/** Parse {@link rowKey}. The profile is the only field that may contain a dash. */
export function parseRow(key: string): Row {
	const parts = key.split("-");
	const [runtime, codec, rate] = parts;
	const ring = parts.at(-1);
	const profile = parts.slice(3, -1).join("-");
	const hz = Number.parseInt(rate ?? "", 10);
	if (
		runtime !== "chromium" ||
		(codec !== "opus" && codec !== "aac") ||
		(ring !== "isolated" && ring !== "plain") ||
		!Number.isFinite(hz) ||
		profile === ""
	) {
		throw new Error(`not a row key: ${key}`);
	}
	return { runtime, codec, rate: hz, profile, ring };
}

// ── what the page emits ─────────────────────────────────────────────────────

/** A run of consecutive quanta the ring did not fill, as the output tap saw it. */
export type Gap = {
	/** Where the first missing sample would have played, on the `render` clock. */
	at: Ms;
	/** The missing audio, at the device rate. */
	ms: Ms;
	/** Quanta in it the ring did not fill, short and silent alike. */
	quanta: number;
	/** Of those, the quanta it filled the front of. */
	short: number;
};

/** A change in whether the ring is stalled, stamped on the `render` clock when the page saw it. */
export type Stall = {
	at: Ms;
	stalled: boolean;
};

/** One probe sample: what the page could read at that instant, plus what the tap reported since. */
export type Sample = {
	/** `performance.now()`: the `viewer` clock. */
	at: Ms;
	/** `AudioContext.currentTime`, read at the same instant: the `render` clock. */
	render?: Ms;

	/** `audio.out.timestamp`: the playhead on the `media` clock. Undefined while there is none. */
	timestamp?: Ms;
	/** `audio.out.stalled`: the ring is refilling rather than playing. */
	stalled?: boolean;
	/** `sync.out.delay`: the resolved playout target. */
	delay?: Ms;

	/** `AudioContext.outputLatency`. */
	outputLatency?: Ms;
	/** `AudioContext.baseLatency`. */
	baseLatency?: Ms;
	/** `AudioContext.renderCapacity` average load, Chromium only. */
	renderLoad?: number;

	/** The tap's cumulative quanta rendered since the first audible one. */
	quanta?: number;
	/** The tap's cumulative quanta under {@link SILENCE_RMS}. */
	quiet?: number;
	/** Gaps the tap closed since the previous sample. */
	gaps?: Gap[];
	/** Stall changes since the previous sample. */
	stalls?: Stall[];
};

/** What the page reports once the session is up. */
export type Environment = {
	/** Whether the document is cross-origin isolated, and therefore which ring runs. */
	crossOriginIsolated: boolean;
	/** The transport the session negotiated. Anything but WebTransport never crossed the UDP shaper. */
	transport?: string;
	/** The audio rendition's codec string. */
	codec?: string;
	/** The audio rendition's sample rate, Hz. */
	rate?: number;
	/** The publisher's declared flush span: the `publish_flush` stage. */
	jitter?: Ms;
	/** The AudioContext's rate, Hz, which the render clock counts in. */
	contextRate?: number;
};

// ── what a row reduces to ───────────────────────────────────────────────────

/** The shaper's counters for one direction, as its exit line prints them. */
export type ShaperCounters = {
	packets: number;
	lost: number;
	overflowed: number;
	throttled: number;
	delayed: number;
	reordered: number;
};

/** The shaper's run: its seed, how it exited, and what it did. */
export type Shaper = {
	seed: number | null;
	/** Its exit status. Nonzero means the profile never acted, or forwarding stopped. */
	status: number | null;
	up: ShaperCounters | null;
	down: ShaperCounters | null;
};

/** Why a row cannot be graded. A void row is reported, and fails an enforced run. */
export type Void = {
	/** The assertion that failed. */
	assertion: string;
	/** What was seen instead. */
	detail: string;
};

/** Everything one row produced: the graded numbers and what makes them trustworthy. */
export type Summary = {
	/** Bumped when a metric's meaning changes, not when one is added. */
	version: 1;
	row: Row;
	/** Graded window, after the warmup. */
	windowMs: Ms;
	/** Discarded after first audio, before grading. */
	warmupMs: Ms;
	environment: Environment | null;
	/** Empty means the row is gradeable. */
	voids: Void[];
	/** Every {@link METRIC_KEYS} entry. Null is unmeasured, never zero. */
	metrics: Record<string, number | null>;
	stages: Record<Stage, StageSpan>;
	/** The delay the stages sum to, or null when no clock spans the whole path. */
	endToEnd: Ms | null;
	drift: Drift[];
	shaper: Shaper | null;
	/** Console warnings and errors, truncated. */
	notes: string[];
};

/** One budget: the whole row it applies to, and a ceiling per graded key. */
export type Budget = Row & { [key: string]: number | string };

/** The checked-in budget file. */
export type Budgets = {
	/** How the ceilings were arrived at. */
	note: string;
	rows: Budget[];
};
