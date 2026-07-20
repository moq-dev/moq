// Opus sample rate constraints, mirroring `pick_opus_rate` in rs/moq-audio/src/codec.rs so the Rust
// and JS publishers advertise the same rates.
//
// Opus runs at a fixed set of rates and nothing else. Audio captured at 44.1 kHz is resampled to
// 48 kHz before it reaches the codec, and the bitstream carries no trace of the original rate, so
// 44100 is never a valid decoder config. Chrome hides this by ignoring the configured rate; Safari
// trusts it and fails every decode.

/** Full-band Opus: its highest rate, and the one to use when the source rate is unknown. */
export const DEFAULT_SAMPLE_RATE = 48_000;

/**
 * The sample rates Opus can encode and decode at, ascending.
 *
 * Frozen because `pickRate` and `supportsRate` read this same array.
 */
export const SAMPLE_RATES: readonly number[] = Object.freeze([8_000, 12_000, 16_000, 24_000, DEFAULT_SAMPLE_RATE]);

/** Whether Opus can be configured at this sample rate. */
export function supportsRate(rate: number): boolean {
	return SAMPLE_RATES.includes(rate);
}

/**
 * Snap an arbitrary sample rate up to the nearest rate Opus supports, falling back to 48 kHz for
 * anything above the highest. Snapping up rather than down avoids throwing away bandwidth the
 * source actually had.
 */
export function pickRate(rate: number): number {
	return SAMPLE_RATES.find((r) => r >= rate) ?? DEFAULT_SAMPLE_RATE;
}
