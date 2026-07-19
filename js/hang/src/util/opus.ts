const SAMPLE_RATES = [8_000, 12_000, 16_000, 24_000, 48_000] as const;

/** Normalize to an Opus output rate, rounding up when possible and capping at 48 kHz. */
export function normalizeSampleRate(sampleRate?: number): number {
	if (sampleRate === undefined) return 48_000;
	if (!Number.isFinite(sampleRate) || sampleRate <= 0) throw new Error("invalid Opus sample rate");

	return SAMPLE_RATES.find((candidate) => candidate >= sampleRate) ?? 48_000;
}
