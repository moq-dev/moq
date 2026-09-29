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

const OPUS_HEAD = new TextEncoder().encode("OpusHead");

function header(description: Uint8Array): { offset: number; littleEndian: boolean } {
	let offset = 0;
	let littleEndian = false;

	if (description.length >= OPUS_HEAD.length && OPUS_HEAD.every((byte, index) => description[index] === byte)) {
		offset = OPUS_HEAD.length;
		littleEndian = true;
	} else if (description[0] === 1) {
		// Some callers already strip the OpusHead signature.
		littleEndian = true;
	} else if (description[0] !== 0) {
		throw new Error("invalid Opus decoder description");
	}

	if (description.length - offset < 11) {
		throw new Error("Opus decoder description must contain at least 11 bytes");
	}

	return { offset, littleEndian };
}

/** Read the codec pre-skip in 48 kHz frames from an OpusHead or dOps payload. */
export function preSkip(description: Uint8Array): number {
	const { offset, littleEndian } = header(description);
	return new DataView(description.buffer, description.byteOffset + offset, 11).getUint16(2, littleEndian);
}

/**
 * Convert an OpusHead decoder description into an ISO BMFF dOps payload.
 *
 * Existing dOps payloads pass through unchanged so CMAF descriptions can be
 * remuxed without another format conversion.
 */
export function toDOps(description: Uint8Array): Uint8Array {
	const { offset, littleEndian } = header(description);

	if (!littleEndian) {
		return description.slice(offset);
	}

	const input = new DataView(description.buffer, description.byteOffset + offset, 11);
	const output = description.slice(offset);
	const view = new DataView(output.buffer);
	output[0] = 0; // dOps version
	output[1] = input.getUint8(1);
	view.setUint16(2, input.getUint16(2, true), false);
	view.setUint32(4, input.getUint32(4, true), false);
	view.setInt16(8, input.getInt16(8, true), false);
	output[10] = input.getUint8(10);
	return output;
}

/**
 * Number of 48 kHz samples in an Opus packet, read from its TOC byte (RFC 6716 §3.1).
 *
 * Mirrors `packet_samples` in rs/moq-mux. Opus timing is always reckoned at 48 kHz regardless of the
 * encoder's internal bandwidth. Undefined for an empty packet or a code-3 packet missing its
 * frame-count byte.
 */
export function packetSamples(packet: Uint8Array): number | undefined {
	const toc = packet.at(0);
	if (toc === undefined) return undefined;

	let frames: number;
	const code = toc & 0b11;
	if (code === 0) frames = 1;
	else if (code !== 3) frames = 2;
	else {
		// Code 3: the frame count is the low 6 bits of the following byte.
		const count = packet.at(1);
		if (count === undefined) return undefined;
		frames = count & 0b11_1111;
	}

	return configSamples(toc >> 3) * frames;
}

// 48 kHz samples per frame for a TOC config index (0..=31), per RFC 6716 Table 1.
function configSamples(config: number): number {
	// SILK NB/MB/WB: 10, 20, 40, 60 ms.
	if (config < 12) return [480, 960, 1920, 2880][config % 4];
	// Hybrid SWB/FB: 10, 20 ms.
	if (config < 16) return [480, 960][config % 2];
	// CELT NB/WB/SWB/FB: 2.5, 5, 10, 20 ms.
	return [120, 240, 480, 960][config % 4];
}
