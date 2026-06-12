// Sampling frequency index table from the MPEG-4 AudioSpecificConfig spec.
const SAMPLE_RATE_INDEX: Record<number, number> = {
	96000: 0,
	88200: 1,
	64000: 2,
	48000: 3,
	44100: 4,
	32000: 5,
	24000: 6,
	22050: 7,
	16000: 8,
	12000: 9,
	11025: 10,
	8000: 11,
	7350: 12,
};

const AAC_LC = 2; // audioObjectType for AAC-LC

// Map a channel count to its AAC channelConfiguration (ISO 14496-3 Table 1.19). Configs 1..=6 are
// identity (5.1 is config 6 / 6 channels); 8 channels is config 7 (7.1). Anything else has no valid
// config, so fall back to stereo (matching the Rust muxer in rs/moq-mux/src/codec/aac).
function channelConfig(channelCount: number): number {
	if (channelCount >= 1 && channelCount <= 6) return channelCount;
	if (channelCount === 8) return 7;
	return 2;
}

// Build the AudioSpecificConfig for AAC-LC, which decoders need to initialize when frames are raw (no
// ADTS header). Standard sample rates produce the 2-byte form; non-table rates fall back to the
// 5-byte form with an explicit 24-bit frequency. This mirrors Config::encode() in the Rust muxer so
// the JS and Rust sides agree on the bytes.
export function audioSpecificConfig(sampleRate: number, channelCount: number): Uint8Array {
	const config = channelConfig(channelCount);
	const freqIndex = SAMPLE_RATE_INDEX[sampleRate];

	if (freqIndex !== undefined) {
		// 5 bits audioObjectType + 4 bits samplingFrequencyIndex + 4 bits channelConfiguration + 3 padding.
		const byte0 = (AAC_LC << 3) | (freqIndex >> 1);
		const byte1 = ((freqIndex & 1) << 7) | (config << 3);
		return new Uint8Array([byte0, byte1]);
	}

	// Escape form: 5 bits AOT + 4 bits 0xF + 24 bits sampleRate + 4 bits channelConfiguration + 3 padding.
	// BigInt keeps the 40-bit field exact (JS bitwise ops are only 32-bit).
	let bits = 0n;
	bits |= BigInt(AAC_LC) << 35n;
	bits |= 0xfn << 31n;
	bits |= BigInt(sampleRate) << 7n;
	bits |= BigInt(config) << 3n;

	const out = new Uint8Array(5);
	for (let i = 0; i < out.length; i++) {
		out[i] = Number((bits >> BigInt((out.length - 1 - i) * 8)) & 0xffn);
	}
	return out;
}
