/**
 * Reduces one row's samples to the {@link Summary} the grader reads.
 *
 * Pure, so every derivation below is unit tested against synthetic samples rather than trusted.
 * `../analyze.ts` is the command that feeds it a run directory.
 *
 * Adapted from `debug-findings/analysis/analyze.mjs` on the reporter's fork (`fperex/moq`, branch
 * `debug/rt-audio`), whose per-minute normalisation this keeps. The event stream
 * it read came from probes patched into the player; this reads public signals and the output tap.
 *
 * @module
 */
import {
	BUCKET_MS,
	type Drift,
	type Environment,
	type Gap,
	MAX_RENDER_DRIFT,
	METRIC_KEYS,
	type Ms,
	type Row,
	type Sample,
	type Shaper,
	STAGES,
	type Stage,
	type StageSpan,
	type Summary,
	type Void,
} from "./schema.ts";

/** Everything a row left behind. */
export type Input = {
	row: Row;
	samples: Sample[];
	environment: Environment | undefined;
	voids: Void[];
	notes: string[];
	shaper: Shaper | null;
	/** Discarded after first audio: every profile spends its first seconds converging on purpose. */
	warmupMs: Ms;
};

/**
 * A gap that starts this close to a stall is the stall, not an underrun.
 *
 * The page learns of a stall after the ring does: up to five quanta late, in the worklet's next
 * state message.
 */
export const STALL_SLACK_MS = 60;

const median = (values: number[]): number | undefined => {
	if (values.length === 0) return undefined;
	const sorted = [...values].sort((a, b) => a - b);
	return sorted[Math.floor(sorted.length / 2)];
};

/** Nearest-rank percentile. Null for an empty series. */
export function percentile(values: number[], p: number): number | null {
	if (values.length === 0) return null;
	const sorted = [...values].sort((a, b) => a - b);
	return sorted[Math.max(0, Math.ceil((p / 100) * sorted.length) - 1)] ?? null;
}

/** Least-squares slope of `ys` over `xs`. Null with nothing to fit. */
export function slope(xs: number[], ys: number[]): number | null {
	const n = xs.length;
	if (n < 2) return null;
	const mx = xs.reduce((a, b) => a + b, 0) / n;
	const my = ys.reduce((a, b) => a + b, 0) / n;
	let num = 0;
	let den = 0;
	for (let i = 0; i < n; i++) {
		num += ((xs[i] ?? 0) - mx) * ((ys[i] ?? 0) - my);
		den += ((xs[i] ?? 0) - mx) ** 2;
	}
	return den === 0 ? null : num / den;
}

/** How far before a disturbance audio's resting point is read from. */
export const REST_MS = 5000;

/** One codec frame of a row, the tolerance for audio being back where it rested. */
export const frameMs = (row: Row): Ms => (row.codec === "aac" ? (1024 / row.rate) * 1000 : BUCKET_MS);

/**
 * The longest time audio took to return after a disturbance, in ms, or null when it never had a
 * resting point to leave. Audio rests at the median lag of the {@link REST_MS} before each sample,
 * leaving out earlier disturbances; one starts at the first sample stalled or more than `frame` off
 * that rest, either way, and ends at the next sample back within `frame` of it. One still open at
 * the end lasts to the last sample. Samples before `from` only set the rest: a disturbance counts
 * once it starts at or after `from`.
 */
export function recovery(
	samples: { at: Ms; lag: Ms | undefined; stalled: boolean }[],
	frame: Ms,
	from: Ms = Number.NEGATIVE_INFINITY,
): Ms | null {
	type Point = (typeof samples)[number];
	// Samples inside a disturbance, which say nothing about where audio rests.
	const disturbed = new Set<number>();
	let longest: Ms | null = null;
	for (let i = 0; i < samples.length; i++) {
		const onset = samples[i] as Point;
		const before = samples.flatMap((s, j) =>
			s.at >= onset.at - REST_MS && j < i && !disturbed.has(j) && !s.stalled && s.lag !== undefined
				? [s.lag]
				: [],
		);
		const rest = median(before);
		if (rest === undefined) continue;
		const counted = onset.at >= from;
		if (counted) longest ??= 0;
		const back = (s: Point) => !s.stalled && s.lag !== undefined && Math.abs(s.lag - rest) <= frame;
		if (back(onset) || (onset.lag === undefined && !onset.stalled)) continue;
		let end = samples.findIndex((s, j) => j > i && back(s));
		if (end < 0) end = samples.length - 1;
		for (let j = i; j < end; j++) disturbed.add(j);
		if (counted) longest = Math.max(longest ?? 0, (samples[end] as Point).at - onset.at);
		i = end;
	}
	return longest;
}

/**
 * Spans in which the ring was stalled, on the render clock.
 *
 * The ring starts stalled, before the page has a context to stamp that on, so a first change to
 * unstalled closes a span that opened before anything was recorded.
 */
export function stallSpans(samples: Sample[]): { start: Ms; end: Ms }[] {
	const changes = samples.flatMap((s) => s.stalls ?? []);
	const spans: { start: Ms; end: Ms }[] = [];
	let open: Ms | undefined = changes[0]?.stalled === false ? Number.NEGATIVE_INFINITY : undefined;
	for (const change of changes) {
		if (change.stalled && open === undefined) open = change.at;
		else if (!change.stalled && open !== undefined) {
			spans.push({ start: open, end: change.at });
			open = undefined;
		}
	}
	if (open !== undefined) spans.push({ start: open, end: Number.POSITIVE_INFINITY });
	return spans;
}

const round = (x: number | null | undefined, places = 1): number | null =>
	x === null || x === undefined || !Number.isFinite(x) ? null : Math.round(x * 10 ** places) / 10 ** places;

/** Reduce one row. */
export function analyze(input: Input): Summary {
	const { row, samples, environment, warmupMs } = input;
	const voids = [...input.voids];
	const notes = [...input.notes];

	// ── the shaper's verdict ────────────────────────────────────────────────
	// A replay row never crosses one: its path is the recording.
	const shaper = input.shaper;
	if (row.runtime === "replay") {
		// Nothing to check.
	} else if (!shaper) {
		voids.push({ assertion: "shaper", detail: "left no report, so nothing says the path was shaped" });
	} else if (shaper.status !== 0) {
		voids.push({ assertion: "shaper", detail: `exited ${shaper.status}: the profile never acted as configured` });
	} else if ((shaper.down?.packets ?? 0) === 0) {
		voids.push({ assertion: "shaper", detail: "forwarded nothing to the page, so the session never crossed it" });
	}

	// ── the window ──────────────────────────────────────────────────────────
	const first = samples.find((s) => s.timestamp !== undefined && !s.stalled);
	const t0 = (first?.at ?? 0) + warmupMs;
	const t1 = samples.at(-1)?.at ?? t0;
	const window = samples.filter((s) => s.at >= t0 && s.at <= t1);
	const minutes = Math.max(1e-9, (t1 - t0) / 60_000);
	if (!first || window.length < 2) {
		voids.push({ assertion: "window", detail: `no audio after the warmup (${samples.length} samples in all)` });
	}

	// ── the render clock, reduced to the viewer's ───────────────────────────
	// Each sample reads both clocks back to back, so the pairs fit the render clock's rate and offset.
	// Fitted from first audio, which is where anything counted on it starts: a native device can sit
	// unpulled for a second while its stream starts, which says nothing about the run after.
	const paired = samples.filter((s) => s.render !== undefined && first !== undefined && s.at >= first.at);
	const renderRate = slope(
		paired.map((s) => s.at),
		paired.map((s) => s.render as Ms),
	);
	const renderOffset = median(paired.map((s) => s.at - (s.render as Ms))) ?? null;
	if (renderRate === null || renderOffset === null) {
		voids.push({ assertion: "clock", detail: "the AudioContext clock was never readable" });
	} else if (Math.abs(renderRate - 1) > MAX_RENDER_DRIFT) {
		voids.push({
			assertion: "clock",
			detail: `the AudioContext clock ran at ${round(renderRate, 3)}x the viewer's: throttled or stalled`,
		});
	}
	const toViewer = (render: Ms) => render + (renderOffset ?? 0);

	// ── underruns, from the tap ─────────────────────────────────────────────
	const stalls = stallSpans(samples);
	const chosen = (gap: Gap) =>
		stalls.some((s) => gap.at >= s.start - STALL_SLACK_MS && gap.at <= s.end + STALL_SLACK_MS);
	const gaps = samples
		.flatMap((s) => s.gaps ?? [])
		.filter((g) => toViewer(g.at) >= t0 && toViewer(g.at) <= t1 && !chosen(g));
	const underruns = gaps.reduce((a, g) => a + g.quanta, 0);
	const short = gaps.reduce((a, g) => a + g.short, 0);
	const underrunMs = gaps.reduce((a, g) => a + g.ms, 0);

	const tapped = window.filter((s) => s.quanta !== undefined && s.quiet !== undefined);
	const firstTap = tapped[0];
	const lastTap = tapped.at(-1);
	const quanta = (lastTap?.quanta ?? 0) - (firstTap?.quanta ?? 0);
	const silence = quanta > 0 ? ((lastTap?.quiet ?? 0) - (firstTap?.quiet ?? 0)) / quanta : null;
	if (!lastTap) notes.push("tap: never reported, so underruns and silence are unmeasured");

	// The player's own counters are cumulative, so the window's count is each one's rise across it.
	const rise = (counter: (s: Sample) => number | undefined) => {
		const counted = window.map(counter).filter((n) => n !== undefined);
		return counted.length > 0 ? (counted.at(-1) ?? 0) - (counted[0] ?? 0) : null;
	};
	const playerUnderruns = rise((s) => s.playerUnderruns);
	const playerSkips = rise((s) => s.playerSkips);
	const trimmed = rise((s) => s.trimmed);
	const accelerated = rise((s) => s.accelerated);

	// ── the playhead ────────────────────────────────────────────────────────
	// The media clock's rate against the viewer's, a property of the source rather than the player.
	// A least-squares fit would read a skip as drift, so the rate is the median slope across spans of
	// eight samples, which a step contaminates only a few of.
	const playing = window.filter((s) => s.timestamp !== undefined && !s.stalled);
	const reach = 8;
	const mediaRate =
		median(
			playing.slice(reach).map((s, i) => {
				const from = playing[i] as Sample;
				return (s.at - (s.timestamp as Ms) - (from.at - (from.timestamp as Ms))) / (s.at - from.at);
			}),
		) ?? null;
	// Both on the media clock, read at one instant, so no reduction to the viewer's is needed.
	const lagOf = (s: Sample) =>
		s.clock === undefined || s.timestamp === undefined ? undefined : s.clock - s.timestamp;
	const behind = playing.flatMap((s) => {
		const lag = lagOf(s);
		return lag === undefined ? [] : [lag];
	});
	// Measured against where audio rested before each disturbance, not zero: a player may settle a
	// little either side of the clock, and how far is `av_lag_ms_p50`'s to grade.
	// The warmup's last `REST_MS` sets where audio rests going into the window, so a disturbance at its
	// start is measured too.
	const recover = recovery(
		samples
			.filter((s) => s.at >= t0 - REST_MS && s.at <= t1)
			.map((s) => ({ at: s.at, lag: s.stalled ? undefined : lagOf(s), stalled: s.stalled === true })),
		frameMs(row),
		t0,
	);
	const stalledShare = window.length > 0 ? window.filter((s) => s.stalled).length / window.length : null;

	// ── the target ──────────────────────────────────────────────────────────
	const targets = window.flatMap((s) => (s.delay === undefined ? [] : [s.delay]));
	// Measured backwards from the end: a target that settles and moves again has not converged.
	const series = samples.filter((s) => first && s.at >= first.at && s.delay !== undefined);
	const final = series.at(-1)?.delay;
	let settled = series[0];
	for (let i = series.length - 1; i >= 0; i--) {
		if (Math.abs((series[i]?.delay ?? 0) - (final ?? 0)) > BUCKET_MS) {
			settled = series[i + 1];
			break;
		}
	}
	const converge = first && settled ? settled.at - first.at : null;

	const loads = window.flatMap((s) => (s.renderLoad === undefined ? [] : [s.renderLoad]));

	const metrics: Record<string, number | null> = Object.fromEntries(METRIC_KEYS.map((k) => [k, null]));
	const perMin = (total: number) => round(total / minutes);
	Object.assign(metrics, {
		underruns_total: lastTap ? underruns : null,
		underruns_per_min: lastTap ? perMin(underruns) : null,
		short_quanta_total: lastTap ? short : null,
		short_quanta_per_min: lastTap ? perMin(short) : null,
		underrun_episodes_total: lastTap ? gaps.length : null,
		underrun_episodes_per_min: lastTap ? perMin(gaps.length) : null,
		underrun_ms_total: lastTap ? round(underrunMs) : null,
		underrun_ms_per_min: lastTap ? perMin(underrunMs) : null,
		underrun_ms_max: lastTap ? round(Math.max(0, ...gaps.map((g) => g.ms))) : null,
		player_underruns_total: playerUnderruns,
		player_underruns_per_min: playerUnderruns === null ? null : perMin(playerUnderruns),
		player_skips_total: playerSkips,
		player_skips_per_min: playerSkips === null ? null : perMin(playerSkips),
		trimmed_ms_total: round(trimmed),
		trimmed_ms_per_min: trimmed === null ? null : perMin(trimmed),
		accelerated_ms_total: round(accelerated),
		accelerated_ms_per_min: accelerated === null ? null : perMin(accelerated),
		stalled_share: round(stalledShare, 3),
		silence_share: round(silence, 3),
		av_lag_ms_p50: round(percentile(behind, 50)),
		av_lag_ms_p95: round(percentile(behind, 95)),
		recover_ms_max: round(recover),
		target_ms_p50: round(percentile(targets, 50)),
		target_ms_p95: round(percentile(targets, 95)),
		target_ms_max: round(percentile(targets, 100)),
		converge_ms_last: round(converge),
		render_load_max: round(percentile(loads, 100), 3),
	});

	// ── stages ──────────────────────────────────────────────────────────────
	// Only the viewer's clocks are readable here, so only the receiver's stages are measured, and no
	// end-to-end exists to sum them to. `publish_flush` is what the publisher declares, reported but
	// never summed with viewer-clock spans.
	const last = window.at(-1);
	const span = (ms: Ms | undefined, source: StageSpan["source"]): StageSpan =>
		ms === undefined ? { ms: null, source: "unmeasured" } : { ms: round(ms), source };
	const known: Partial<Record<Stage, StageSpan>> = {
		publish_flush: span(environment?.jitter, "declared"),
		jitter_buffer: span(last?.delay, "measured"),
		device:
			last?.outputLatency === undefined
				? span(undefined, "unmeasured")
				: span(last.outputLatency + (last.baseLatency ?? 0), "measured"),
	};
	const stages = Object.fromEntries(STAGES.map((s) => [s, known[s] ?? span(undefined, "unmeasured")])) as Record<
		Stage,
		StageSpan
	>;

	const drift: Drift[] = [
		{ clock: "viewer", rate: 0, offset: 0 },
		{ clock: "render", rate: round(renderRate === null ? null : renderRate - 1, 6), offset: round(renderOffset) },
		// The lag's slope is the media clock's rate against the viewer's, negated.
		{ clock: "media", rate: round(mediaRate === null ? null : -mediaRate, 6), offset: null },
		{ clock: "publisher", rate: null, offset: null },
		{ clock: "relay", rate: null, offset: null },
	];

	return {
		version: 1,
		row,
		windowMs: round(t1 - t0) ?? 0,
		warmupMs,
		environment: environment ?? null,
		voids,
		metrics,
		stages,
		endToEnd: null,
		drift,
		shaper,
		notes: notes.slice(0, 50),
	};
}
