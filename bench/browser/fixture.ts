/** Deterministic payloads shared by the pages and the driver, so a skipped or altered frame cannot pass. */

// Bytes sampled per frame in timed runs; counted runs check every byte.
const PROBES = 16;

/** Frame `frame` of a `size`-byte stream: a little-endian frame index, then a pattern keyed on both. */
export function payload(size: number, frame: number): Uint8Array {
	if (size < 4) throw new Error(`payload needs at least 4 bytes, got ${size}`);
	const bytes = new Uint8Array(size);
	new DataView(bytes.buffer).setUint32(0, frame, true);
	for (let i = 4; i < size; i++) bytes[i] = expected(i, frame);
	return bytes;
}

function expected(index: number, frame: number): number {
	return (index * 31 + frame * 7) & 0xff;
}

/**
 * Hashes one received frame, throwing on any mismatch it inspects.
 *
 * Callers sum the hashes, since groups arrive on independent streams in any order.
 */
export function check(bytes: Uint8Array, size: number, frame: number, full: boolean): number {
	if (bytes.byteLength !== size) throw new Error(`frame ${frame}: ${bytes.byteLength} bytes, expected ${size}`);
	const index = new DataView(bytes.buffer, bytes.byteOffset, 4).getUint32(0, true);
	if (index !== frame) throw new Error(`frame ${frame}: carries index ${index}`);
	let hash = mix(OFFSET, frame);
	if (full) {
		for (let i = 4; i < size; i++) {
			if (bytes[i] !== expected(i, frame)) throw new Error(`frame ${frame}: incorrect byte ${i}`);
		}
	}
	for (let probe = 0; probe < PROBES; probe++) {
		const i = 4 + Math.floor((probe * (size - 5)) / (PROBES - 1));
		if (bytes[i] !== expected(i, frame)) throw new Error(`frame ${frame}: incorrect byte ${i}`);
		hash = mix(hash, bytes[i]);
	}
	return hash;
}

/** The summed hashes of a complete stream of `frames` frames. */
export function digest(size: number, frames: number): number {
	let sum = 0;
	for (let frame = 0; frame < frames; frame++) sum = (sum + check(payload(size, frame), size, frame, false)) >>> 0;
	return sum;
}

// FNV-1a over 32 bits.
const OFFSET = 0x811c9dc5;

function mix(hash: number, value: number): number {
	return Math.imul(hash ^ value, 0x01000193) >>> 0;
}
