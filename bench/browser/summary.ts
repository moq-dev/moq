/** Summaries and paired comparisons over raw samples. Descriptive only: nothing here fails a run. */

/** One retained sample. `round` is -1 for the single traced, counted, or probed pass. */
export interface Row {
	scenario: string;
	key: string;
	shape: Record<string, unknown>;
	variant: string;
	round: number;
	pass: "timed" | "traced" | "counted" | "probed";
	delivered: { frames: number; bytes: number; digest: number };
	metrics: Record<string, number>;
	heap: { before: number | null; after: number | null };
	load: number[];
}

/** Median, minimum and maximum of each metric, per workload and variant. */
export function summarize(rows: Row[]) {
	const groups = new Map<string, Row[]>();
	for (const row of rows) {
		const id = `${row.key}\0${row.variant}`;
		groups.set(id, [...(groups.get(id) ?? []), row]);
	}
	return [...groups.values()].map((group) => ({
		key: group[0].key,
		variant: group[0].variant,
		samples: group.length,
		metrics: Object.fromEntries(
			Object.keys(group[0].metrics).map((metric) => [metric, range(group.map((row) => row.metrics[metric]))]),
		),
	}));
}

/**
 * Each `[variant, reference]` pair, matched by round, as percentage deltas of the variant.
 *
 * Pairs share a round, so slow drift in host load lands on both sides. The median and range
 * are descriptive; a sign that flips between pairs says the change is within the noise.
 */
export function compare(rows: Row[], pairs: [string, string][]) {
	const byRound = new Map(rows.map((row) => [`${row.key}\0${row.variant}\0${row.round}`, row]));
	const deltas = new Map<
		string,
		{ key: string; variant: string; reference: string; metric: string; values: number[] }
	>();
	for (const [variant, reference] of pairs) {
		for (const row of rows) {
			if (row.variant !== variant) continue;
			const base = byRound.get(`${row.key}\0${reference}\0${row.round}`);
			if (!base) continue;
			for (const [metric, value] of Object.entries(row.metrics)) {
				const before = base.metrics[metric];
				if (before === undefined || before === 0 || !Number.isFinite(value)) continue;
				const id = `${row.key}\0${variant}\0${reference}\0${metric}`;
				const entry = deltas.get(id) ?? { key: row.key, variant, reference, metric, values: [] };
				entry.values.push(((value - before) / before) * 100);
				deltas.set(id, entry);
			}
		}
	}
	return [...deltas.values()].map(({ values, ...entry }) => {
		const { min, median, max } = range(values);
		return {
			...entry,
			pairs: values.length,
			median_pct: median,
			min_pct: min,
			max_pct: max,
			mad_pct: range(values.map((value) => Math.abs(value - median))).median,
			consistent: values.every((value) => Math.sign(value) === Math.sign(median)),
		};
	});
}

/** Minimum, median and maximum; the median of an even count averages the middle two. */
export function range(values: number[]) {
	const sorted = [...values].sort((a, b) => a - b);
	const middle = Math.floor(sorted.length / 2);
	return {
		min: sorted[0],
		median: sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2,
		max: sorted[sorted.length - 1],
	};
}
