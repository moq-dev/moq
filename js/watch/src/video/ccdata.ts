import type { Duration } from "@moq/clock";

/** One CEA-608/708 pair parsed from A/53 cc_data (ATSC A/53, GA94 SEI). */
export interface CcDataPair {
	valid: boolean;
	type: number;
	data1: number;
	data2: number;
}

/** Coded-video caption data emitted when {@link DecoderInput.emitSei} is on. */
export interface CcDataEvent {
	/** Presentation timestamp of the encoded sample. */
	timestamp: Duration;
	pairs: CcDataPair[];
	payloadBytes: number;
}

/**
 * Pull A/53 GA94 `cc_data` pairs out of an Annex-B or 4-byte length-prefixed
 * (AVCC/HVCC) access unit. Exported so a page or test can reuse the same parse
 * the decoder uses when `emitSei` is on.
 */
export function extractCcData(buf: Uint8Array, codec: "avc" | "hevc" = "avc"): CcDataPair[] {
	const nals = looksLikeAnnexB(buf)
		? splitAnnexB(buf)
		: looksLikeLengthPrefixed(buf)
			? splitLengthPrefixed(buf)
			: splitAnnexB(buf);
	const pairs: CcDataPair[] = [];
	for (const nal of nals) pairs.push(...extractCcDataFromNal(nal, codec));
	return pairs;
}

// A 4-byte Annex-B start code is `00 00 00 01`, which is also a valid 4-byte length of 1.
// Prefer the start-code read: access units do not begin with a 1-byte NAL, and a length-first
// heuristic would swallow a normal Annex-B AU as a 1-byte NAL and drop the SEI.
function looksLikeAnnexB(buf: Uint8Array): boolean {
	if (buf.length >= 3 && buf[0] === 0 && buf[1] === 0 && buf[2] === 1) return true;
	return buf.length >= 4 && buf[0] === 0 && buf[1] === 0 && buf[2] === 0 && buf[3] === 1;
}

function looksLikeLengthPrefixed(buf: Uint8Array): boolean {
	if (buf.length < 4 || looksLikeAnnexB(buf)) return false;
	const size = ((buf[0] << 24) | (buf[1] << 16) | (buf[2] << 8) | buf[3]) >>> 0;
	return size > 0 && size + 4 <= buf.length;
}

function splitAnnexB(buf: Uint8Array): Uint8Array[] {
	const nals: Uint8Array[] = [];
	let i = 0;
	while (i + 3 < buf.length) {
		const sc3 = buf[i] === 0 && buf[i + 1] === 0 && buf[i + 2] === 1;
		const sc4 = i + 4 <= buf.length && buf[i] === 0 && buf[i + 1] === 0 && buf[i + 2] === 0 && buf[i + 3] === 1;
		if (!sc3 && !sc4) {
			i += 1;
			continue;
		}
		const start = i + (sc4 ? 4 : 3);
		let j = start;
		while (j + 3 < buf.length) {
			const n3 = buf[j] === 0 && buf[j + 1] === 0 && buf[j + 2] === 1;
			const n4 = j + 4 <= buf.length && buf[j] === 0 && buf[j + 1] === 0 && buf[j + 2] === 0 && buf[j + 3] === 1;
			if (n3 || n4) break;
			j += 1;
		}
		if (j + 3 >= buf.length) j = buf.length;
		nals.push(buf.subarray(start, j));
		i = j;
	}
	return nals;
}

function splitLengthPrefixed(buf: Uint8Array): Uint8Array[] {
	const nals: Uint8Array[] = [];
	let i = 0;
	while (i + 4 <= buf.length) {
		const size = ((buf[i] << 24) | (buf[i + 1] << 16) | (buf[i + 2] << 8) | buf[i + 3]) >>> 0;
		i += 4;
		if (size <= 0 || i + size > buf.length) break;
		nals.push(buf.subarray(i, i + size));
		i += size;
	}
	return nals;
}

function stripEmulationPrevention(nal: Uint8Array): Uint8Array {
	const out: number[] = [];
	for (let i = 0; i < nal.length; i++) {
		if (
			nal[i] === 0x03 &&
			i >= 2 &&
			nal[i - 1] === 0 &&
			nal[i - 2] === 0 &&
			i + 1 < nal.length &&
			nal[i + 1] <= 0x03
		) {
			continue;
		}
		out.push(nal[i]);
	}
	return Uint8Array.from(out);
}

function readSeiPayloads(rbsp: Uint8Array): Array<{ type: number; payload: Uint8Array }> {
	const out: Array<{ type: number; payload: Uint8Array }> = [];
	let i = 0;
	while (i < rbsp.length) {
		let type = 0;
		while (i < rbsp.length && rbsp[i] === 0xff) {
			type += 255;
			i += 1;
		}
		if (i >= rbsp.length) break;
		type += rbsp[i++];
		let length = 0;
		while (i < rbsp.length && rbsp[i] === 0xff) {
			length += 255;
			i += 1;
		}
		if (i >= rbsp.length) break;
		length += rbsp[i++];
		if (i + length > rbsp.length) break;
		out.push({ type, payload: rbsp.subarray(i, i + length) });
		i += length;
	}
	return out;
}

function parseAtscUserData(payload: Uint8Array): CcDataPair[] {
	// ITU-T T.35 country 0xB5, provider 0x0031, 'GA94', type 0x03
	if (payload.length < 8) return [];
	if (payload[0] !== 0xb5) return [];
	if (((payload[1] << 8) | payload[2]) !== 0x0031) return [];
	if (payload[3] !== 0x47 || payload[4] !== 0x41 || payload[5] !== 0x39 || payload[6] !== 0x34) return [];
	if (payload[7] !== 0x03) return [];
	if (payload.length < 10) return [];
	const count = payload[8] & 0x1f;
	const pairs: CcDataPair[] = [];
	let o = 9;
	for (let n = 0; n < count && o + 2 < payload.length; n++) {
		const marker = payload[o];
		pairs.push({
			valid: (marker & 0x04) !== 0,
			type: marker & 0x03,
			data1: payload[o + 1],
			data2: payload[o + 2],
		});
		o += 3;
	}
	return pairs;
}

function extractCcDataFromNal(raw: Uint8Array, codec: "avc" | "hevc"): CcDataPair[] {
	if (!raw.length) return [];
	let isSei = false;
	let headerSize = 1;
	if (codec === "hevc") {
		if (raw.length < 2) return [];
		const nalType = (raw[0] >> 1) & 0x3f;
		isSei = nalType === 39 || nalType === 40;
		headerSize = 2;
	} else {
		isSei = (raw[0] & 0x1f) === 6;
		headerSize = 1;
	}
	if (!isSei) return [];
	const rbsp = stripEmulationPrevention(raw.subarray(headerSize));
	const pairs: CcDataPair[] = [];
	for (const msg of readSeiPayloads(rbsp)) {
		if (msg.type === 4) pairs.push(...parseAtscUserData(msg.payload));
	}
	return pairs;
}
