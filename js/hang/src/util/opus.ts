// Opus sample rate constraints, mirroring rs/moq-audio/src/opus.rs so the Rust and JS publishers
// advertise the same rates.
//
// Opus runs at a fixed set of rates and nothing else. Audio captured at 44.1 kHz is resampled to
// 48 kHz before it reaches the codec, and the bitstream carries no trace of the original rate, so
// 44100 is never a valid decoder config. Chrome hides this by ignoring the configured rate; Safari
// trusts it and fails every decode.

// Sample rates Opus runs at, ascending.
const RATES = [8_000, 12_000, 16_000, 24_000, 48_000];

/** The sample rates Opus can encode and decode at, ascending. */
export const SAMPLE_RATES: readonly number[] = RATES;

/** Whether Opus can be configured at this sample rate. */
export function supportsRate(rate: number): boolean {
	return RATES.includes(rate);
}

/**
 * Snap an arbitrary sample rate up to the nearest rate Opus supports, falling back to 48 kHz for
 * anything above the highest. Snapping up rather than down avoids throwing away bandwidth the
 * source actually had.
 */
export function pickRate(rate: number): number {
	return RATES.find((r) => r >= rate) ?? 48_000;
}
