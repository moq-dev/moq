/**
 * Reduces one row's samples to the {@link Summary} the grader reads.
 *
 * Pure, so every derivation below is unit tested against synthetic samples rather than trusted.
 * `../analyze.ts` is the command that feeds it a run directory.
 *
 * Adapted from `debug-findings/analysis/analyze.mjs` on the reporter's fork (`fperex/moq`, branch
 * `debug/rt-audio`), whose skip-ahead rule and per-minute normalisation this keeps. The event stream
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
 * The page learns of a stall after the ring does: up to five quanta late on the postMessage ring,
 * one poll late on the isolated one.
 */
export const STALL_SLACK_MS = 60;

/** A step in the de-trended playhead lag larger than this is a skip-ahead. See {@link skipAheads}. */
export const SKIP_MS = 40;

/** Samples either side of a candidate skip reduced to a median before it is judged. */
export const SKIP_WINDOW = 4;

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

/**
 * Forward steps in the lag between the viewer clock and the playhead, and the media they skipped.
 *
 * `lags` is the de-trended `at - timestamp` per sample, undefined where there was no playhead or the
 * ring was stalled. The playhead a page reads is quantized (it moves 240 to 296 ms per 250 ms sample
 * on a clean path), so one sample's excess is noise: a skip is a step between the medians either side
 * of a sample, larger than {@link SKIP_MS}, which is more than that band and more than one estimator
 * bucket plus a render quantum.
 */
export function skipAheads(lags: (Ms | undefined)[]): { count: number; ms: Ms } {
	let count = 0;
	let ms = 0;
	const defined = (slice: (Ms | undefined)[]) => slice.filter((l): l is Ms => l !== undefined);
	for (let i = SKIP_WINDOW; i < lags.length - SKIP_WINDOW; i++) {
		if (lags[i] === undefined) continue;
		const before = median(defined(lags.slice(i - SKIP_WINDOW, i)));
		const after = median(defined(lags.slice(i + 1, i + 1 + SKIP_WINDOW)));
		if (before === undefined || after === undefined) continue;
		const jumped = before - after;
		if (jumped > SKIP_MS) {
			count++;
			ms += jumped;
			// One step is one skip, and the trailing window straddles it for several samples.
			i += SKIP_WINDOW;
		}
	}
	return { count, ms };
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
	const paired = samples.filter((s) => s.render !== undefined);
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

	// ── the playhead ────────────────────────────────────────────────────────
	// The lag drifts when the media timeline does not run at wall rate, a property of the source
	// rather than the player. Fitted and removed first, or it reads as a skip every few seconds.
	// A least-squares fit would read a skip itself as drift and hide part of it, so the rate is the
	// median slope across spans of two skip windows, which a step contaminates only a few of.
	const playing = window.filter((s) => s.timestamp !== undefined && !s.stalled);
	const reach = 2 * SKIP_WINDOW;
	const mediaRate =
		median(
			playing.slice(reach).map((s, i) => {
				const from = playing[i] as Sample;
				return (s.at - (s.timestamp as Ms) - (from.at - (from.timestamp as Ms))) / (s.at - from.at);
			}),
		) ?? null;
	const base = playing[0]?.at ?? 0;
	const lags = window.map((s) =>
		s.timestamp === undefined || s.stalled ? undefined : s.at - s.timestamp - (mediaRate ?? 0) * (s.at - base),
	);
	const skips = skipAheads(lags);
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
		skip_aheads_total: skips.count,
		skip_aheads_per_min: perMin(skips.count),
		discarded_ms_total: round(skips.ms),
		discarded_ms_per_min: perMin(skips.ms),
		stalled_share: round(stalledShare, 3),
		silence_share: round(silence, 3),
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
