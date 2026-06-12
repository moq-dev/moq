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

// Build the 2-byte AudioSpecificConfig for AAC-LC, which decoders need to initialize when frames are
// raw (no ADTS header). Layout: 5 bits audioObjectType + 4 bits samplingFrequencyIndex + 4 bits
// channelConfiguration + 3 bits GASpecificConfig (all zero for AAC-LC).
export function audioSpecificConfig(sampleRate: number, channelCount: number): Uint8Array {
	const freqIndex = SAMPLE_RATE_INDEX[sampleRate] ?? 4; // default to 44100 if unknown
	const audioObjectType = 2; // AAC-LC

	const byte0 = (audioObjectType << 3) | (freqIndex >> 1);
	const byte1 = ((freqIndex & 1) << 7) | (channelCount << 3);

	return new Uint8Array([byte0, byte1]);
}
