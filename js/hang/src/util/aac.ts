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

/**
 * The sample rates the AAC sampling frequency table lists, ascending.
 *
 * Derived from the index table above so the two can't drift, and frozen because `pickRate` reads it.
 */
export const SAMPLE_RATES: readonly number[] = Object.freeze(
	Object.keys(SAMPLE_RATE_INDEX)
		.map(Number)
		.sort((a, b) => a - b),
);

/** Whether the sample rate is one the AAC sampling frequency table lists. */
export function supportsRate(rate: number): boolean {
	return SAMPLE_RATE_INDEX[rate] !== undefined;
}

/**
 * Snap an arbitrary sample rate up to the nearest rate in the AAC sampling frequency table, falling
 * back to the highest (96 kHz) for anything above it.
 *
 * `audioSpecificConfig` can describe an off-table rate via its escape form, but encoders only
 * accept the table rates, so capture at one of those instead of relying on the escape form.
 */
export function pickRate(rate: number): number {
	return SAMPLE_RATES.find((r) => r >= rate) ?? SAMPLE_RATES[SAMPLE_RATES.length - 1];
}

const AAC_LC = 2; // audioObjectType for AAC-LC

// The WebCodecs AAC registration's spellings of the object types we decode, matched exactly so an
// unlisted spelling (such as `mp4a.40.0005`) is refused rather than guessed. MPEG-2 AAC-LC
// (`mp4a.67`) is the same bitstream as AAC-LC.
const OBJECT_TYPES: ReadonlyMap<string, 2 | 5 | 29> = new Map([
	["mp4a.40.2", AAC_LC],
	["mp4a.40.02", AAC_LC],
	["mp4a.67", AAC_LC],
	["mp4a.40.5", 5],
	["mp4a.40.05", 5],
	["mp4a.40.29", 29],
]);

/**
 * The audioObjectType a WebCodecs AAC codec string names: AAC-LC (2), HE-AAC (5) or HE-AACv2 (29).
 * Undefined for any other codec, including xHE-AAC (`mp4a.40.42`).
 */
export function objectType(codec: string): 2 | 5 | 29 | undefined {
	return OBJECT_TYPES.get(codec);
}

// Map a channel count to its AAC channelConfiguration (ISO 14496-3 Table 1.19). Configs 1..=6 are
// identity (5.1 is config 6 / 6 channels); 8 channels is config 7 (7.1). Anything else has no
// config, and describing it as stereo would mislabel every channel past the first two.
function channelConfig(channelCount: number): number {
	if (Number.isInteger(channelCount) && channelCount >= 1 && channelCount <= 6) return channelCount;
	if (channelCount === 8) return 7;
	throw new Error(`no AAC channel configuration for ${channelCount} channels`);
}

// The sampling frequency table by index, from the MPEG-4 AudioSpecificConfig spec.
const SAMPLE_RATE_TABLE: readonly number[] = Object.keys(SAMPLE_RATE_INDEX)
	.map(Number)
	.sort((a, b) => SAMPLE_RATE_INDEX[a] - SAMPLE_RATE_INDEX[b]);

// The channel count each channelConfiguration names (ISO 14496-3 Table 1.19); 0 defers to a
// program config element, and 8, 9, 10, and 15 are reserved.
const CHANNEL_COUNTS: Readonly<Record<number, number>> = {
	1: 1,
	2: 2,
	3: 3,
	4: 4,
	5: 5,
	6: 6,
	7: 8,
	11: 7,
	12: 8,
	13: 24,
	14: 8,
};

/** The output an AudioSpecificConfig decodes to. */
export interface Config {
	/** The decoded rate: under SBR, the extension rate rather than the core's. */
	sampleRate: number;
	/** The decoded channels: two under PS, which a mono core carries. */
	numberOfChannels: number;
}

/**
 * Read the decoded sample rate and channel count from an AudioSpecificConfig (ISO 14496-3 §1.6.2.1).
 *
 * Mirrors `aac::Config::parse` in rs/moq-mux for explicitly signaled SBR and PS. A backward-compatible
 * sync extension isn't followed, so it reports the core. Undefined when the config is truncated,
 * names a reserved rate or channelConfiguration, or takes its channels from a program config element.
 */
export function parseConfig(asc: Uint8Array): Config | undefined {
	let bit = 0;
	const read = (n: number): number | undefined => {
		if (bit + n > asc.byteLength * 8) return undefined;
		let value = 0;
		for (const end = bit + n; bit < end; bit++) value = value * 2 + ((asc[bit >> 3] >> (7 - (bit & 7))) & 1);
		return value;
	};
	// audioObjectType, escaped to 32 and above by 31 and a 6-bit extension.
	const objectType = (): number | undefined => {
		const type = read(5);
		if (type !== 31) return type;
		const extension = read(6);
		return extension === undefined ? undefined : 32 + extension;
	};
	// samplingFrequencyIndex, escaped to an explicit 24-bit rate by 15.
	const sampleRate = (): number | undefined => {
		const index = read(4);
		if (index !== 15) return index === undefined ? undefined : SAMPLE_RATE_TABLE[index];
		const rate = read(24);
		return rate === 0 ? undefined : rate;
	};

	const type = objectType();
	let rate = sampleRate();
	const channelConfig = read(4);
	if (type === undefined || channelConfig === undefined) return undefined;
	// Explicit SBR (5) or PS (29) plays at the extension rate that follows.
	if (type === 5 || type === 29) rate = sampleRate();
	const numberOfChannels = type === 29 ? 2 : CHANNEL_COUNTS[channelConfig];
	if (rate === undefined || numberOfChannels === undefined) return undefined;
	return { sampleRate: rate, numberOfChannels };
}

/**
 * Build the AAC-LC AudioSpecificConfig that decoders need when frames are raw (no ADTS header).
 *
 * Standard sample rates produce the 2-byte form; non-table rates fall back to the 5-byte form
 * with an explicit 24-bit frequency. Throws for a channel count no configuration names.
 */
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
