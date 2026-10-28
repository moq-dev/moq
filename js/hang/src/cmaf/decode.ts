/**
 * MP4 decoding utilities for parsing fMP4 init and data segments.
 * Used by WebCodecs to extract raw frames from CMAF container.
 */

import type { Time } from "@moq/net";

/**
 * One ISO BMFF box: its four-character type and its payload, a view into the input.
 */
interface Box {
	type: string;
	/** Where the box starts in the data it was read from. */
	offset: number;
	payload: Uint8Array;
}

/**
 * The boxes `data` holds back to back, each checked to fit inside it, so a declared size can
 * neither stall the walk nor reach past its parent.
 */
function boxes(data: Uint8Array): Box[] {
	const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
	const result: Box[] = [];
	let offset = 0;
	while (offset < data.byteLength) {
		const remaining = data.byteLength - offset;
		if (remaining < 8) throw new Error(`Truncated box header: ${remaining} bytes left`);
		const type = String.fromCharCode(...data.subarray(offset + 4, offset + 8));
		let header = 8;
		let size = view.getUint32(offset);
		if (size === 1) {
			if (remaining < 16) throw new Error(`Truncated ${type} box header`);
			const large = view.getBigUint64(offset + 8);
			if (large > BigInt(remaining)) throw new Error(`${type} box size ${large} exceeds its ${remaining} bytes`);
			size = Number(large);
			header = 16;
		} else if (size === 0) {
			// Extends to the end of its parent.
			size = remaining;
		}
		if (type === "uuid") header += 16;
		if (size < header || size > remaining) {
			throw new Error(`Invalid ${type} box size ${size}: ${remaining} bytes left, header ${header}`);
		}
		result.push({ type, offset, payload: data.subarray(offset + header, offset + size) });
		offset += size;
	}
	return result;
}

/** The first box of `type` among `list`. */
function child(list: Box[], type: string): Box | undefined {
	return list.find((box) => box.type === type);
}

/** The first box of `type` among `list`, or an error naming where it was missing. */
function required(list: Box[], type: string, where: string): Box {
	const box = child(list, type);
	if (!box) throw new Error(`No ${type} box found in ${where}`);
	return box;
}

/** Reads a box payload front to back, refusing to read past its end. */
class Reader {
	readonly type: string;
	readonly #view: DataView;
	readonly #length: number;
	#offset = 0;

	constructor(payload: Uint8Array, type: string) {
		this.type = type;
		this.#view = new DataView(payload.buffer, payload.byteOffset, payload.byteLength);
		this.#length = payload.byteLength;
	}

	get remaining(): number {
		return this.#length - this.#offset;
	}

	#take(bytes: number): number {
		if (bytes > this.remaining) throw new Error(`Truncated ${this.type} box`);
		const at = this.#offset;
		this.#offset += bytes;
		return at;
	}

	skip(bytes: number): void {
		this.#take(bytes);
	}

	u8(): number {
		return this.#view.getUint8(this.#take(1));
	}

	u24(): number {
		const at = this.#take(3);
		return (this.#view.getUint8(at) << 16) | this.#view.getUint16(at + 1);
	}

	u32(): number {
		return this.#view.getUint32(this.#take(4));
	}

	i32(): number {
		return this.#view.getInt32(this.#take(4));
	}

	u64(): number {
		const value = this.#view.getBigUint64(this.#take(8));
		if (value > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error(`${this.type} value ${value} exceeds 2^53`);
		return Number(value);
	}

	/** The version and flags of a full box. */
	full(): { version: number; flags: number } {
		return { version: this.u8(), flags: this.u24() };
	}
}

/** Visual sample entries whose codec configuration this decoder extracts. */
const VISUAL_ENTRIES = new Set(["avc1", "avc3", "hvc1", "hev1"]);
/** Audio sample entries whose codec configuration this decoder extracts. */
const AUDIO_ENTRIES = new Set(["mp4a"]);

/**
 * Result of parsing an init segment.
 */
export interface InitSegment {
	/** Codec-specific description (avcC, hvcC, esds, dOps, etc.) */
	description?: Uint8Array;
	/** Time units per second */
	timescale: number;
	/** Track ID from the init segment */
	trackId: number;
	/** Whether the track carries audio or video. */
	kind: "audio" | "video";
	/** Default sample duration from moov/mvex/trex, used when not overridden in tfhd/trun. */
	defaultSampleDuration: number;
	/** Default sample size from moov/mvex/trex, used when not overridden in tfhd/trun. */
	defaultSampleSize: number;
	/**
	 * Default sample flags from moov/mvex/trex, used when not overridden in tfhd/trun.
	 *
	 * Some encoders (notably gstreamer/ffmpeg passthrough) only set sample flags
	 * in trex, leaving tfhd defaults and per-sample trun flags zero. Without this
	 * fallback every sample would appear as a sync sample.
	 */
	defaultSampleFlags: number;
}

/**
 * A decoded sample from a data segment.
 */
export interface Sample {
	/** Raw sample data */
	data: Uint8Array;
	/** Timestamp in microseconds */
	timestamp: number;
	/** Whether this is a video keyframe (sync sample). Always false for audio; see {@link InitSegment.kind}. */
	keyframe: boolean;
	/** Sample duration in microseconds (0 when the fragment doesn't carry one) */
	duration: number;
}

/**
 * Parse an init segment (ftyp + moov) to extract codec description and timescale.
 *
 * @param init - The init segment data
 * @returns Parsed init segment information
 */
export function decodeInitSegment(init: Uint8Array): InitSegment {
	const moov = boxes(required(boxes(init), "moov", "init segment").payload);
	const trak = boxes(required(moov, "trak", "init segment").payload);
	const mdia = boxes(required(trak, "mdia", "init segment").payload);

	// moov > trak > mdia > mdhd for the timescale.
	const mdhd = new Reader(required(mdia, "mdhd", "init segment").payload, "mdhd");
	const { version: mdhdVersion } = mdhd.full();
	mdhd.skip(mdhdVersion === 1 ? 16 : 8);
	const timescale = mdhd.u32();
	if (timescale === 0) throw new Error("mdhd timescale is zero");

	// moov > trak > tkhd for the track ID.
	const tkhdBox = child(trak, "tkhd");
	let trackId = 1;
	if (tkhdBox) {
		const tkhd = new Reader(tkhdBox.payload, "tkhd");
		const { version } = tkhd.full();
		tkhd.skip(version === 1 ? 16 : 8);
		trackId = tkhd.u32();
	}

	// moov > trak > mdia > hdlr for the track kind.
	const hdlr = new Reader(required(mdia, "hdlr", "init segment").payload, "hdlr");
	hdlr.full();
	hdlr.skip(4);
	const handlerType = String.fromCharCode(hdlr.u8(), hdlr.u8(), hdlr.u8(), hdlr.u8());
	const kind = handlerType === "soun" ? "audio" : handlerType === "vide" ? "video" : undefined;
	if (!kind) {
		throw new Error(`Unsupported track handler: ${handlerType}`);
	}

	// moov > trak > mdia > minf > stbl > stsd for the sample description.
	const minf = boxes(required(mdia, "minf", "init segment").payload);
	const stbl = boxes(required(minf, "stbl", "init segment").payload);
	const stsd = new Reader(required(stbl, "stsd", "init segment").payload, "stsd");
	stsd.full();
	const entryCount = stsd.u32();
	const entries = boxes(required(stbl, "stsd", "init segment").payload.subarray(8));
	if (entryCount === 0 || entries.length === 0) {
		throw new Error("No stsd box found in init segment");
	}

	// Extract the codec-specific description from the first sample entry.
	const description = extractDescription(entries[0]);

	// moov > mvex > trex for this track's default sample values: the bottom of the fallback chain
	// when tfhd and trun don't specify them.
	const mvex = child(moov, "mvex");
	let trex: { duration: number; size: number; flags: number } | undefined;
	for (const box of mvex ? boxes(mvex.payload) : []) {
		if (box.type !== "trex") continue;
		const reader = new Reader(box.payload, "trex");
		reader.full();
		if (reader.u32() !== trackId) continue;
		reader.skip(4);
		trex = { duration: reader.u32(), size: reader.u32(), flags: reader.u32() };
		break;
	}

	return {
		description,
		timescale,
		trackId,
		kind,
		defaultSampleDuration: trex?.duration ?? 0,
		defaultSampleSize: trex?.size ?? 0,
		defaultSampleFlags: trex?.flags ?? 0,
	};
}

/**
 * Extract codec-specific description from a sample entry.
 * The description is codec-specific: avcC for H.264, hvcC for H.265, esds for AAC.
 */
function extractDescription(entry: Box): Uint8Array | undefined {
	// The SampleEntry and Visual/AudioSampleEntry fields precede the child boxes.
	let fields: number;
	if (VISUAL_ENTRIES.has(entry.type)) fields = 78;
	else if (AUDIO_ENTRIES.has(entry.type)) fields = 28;
	else return undefined;
	if (entry.payload.byteLength < fields) throw new Error(`Truncated ${entry.type} sample entry`);

	for (const box of boxes(entry.payload.subarray(fields))) {
		if (box.type === "avcC" || box.type === "hvcC") {
			return new Uint8Array(box.payload);
		}
		if (box.type === "esds") {
			// esds payload has nested descriptors; extract the AudioSpecificConfig (tag 0x05).
			return extractAudioSpecificConfig(box.payload);
		}
	}

	return undefined;
}

/**
 * Extract AudioSpecificConfig from an esds box payload.
 * The esds contains nested descriptors: ES_Descriptor (0x03) → DecoderConfigDescriptor (0x04)
 * → DecoderSpecificInfo (0x05). The DecoderSpecificInfo payload is the AudioSpecificConfig
 * that AudioDecoder.configure() expects.
 */
function extractAudioSpecificConfig(esds: Uint8Array): Uint8Array | undefined {
	// Skip version + flags (4 bytes)
	let offset = 4;

	// Scan for DecoderSpecificInfo tag (0x05)
	while (offset < esds.length) {
		const tag = esds[offset++];

		// Parse variable-length size (up to 4 bytes, high bit = continuation)
		let size = 0;
		for (let i = 0; i < 4 && offset < esds.length; i++) {
			const b = esds[offset++];
			size = (size << 7) | (b & 0x7f);
			if ((b & 0x80) === 0) break;
		}

		if (tag === 0x05) {
			// Found DecoderSpecificInfo — payload is the AudioSpecificConfig
			if (offset + size <= esds.length) {
				return new Uint8Array(esds.buffer, esds.byteOffset + offset, size);
			}
			return undefined;
		}

		// For container descriptors (0x03, 0x04), skip their fixed header fields
		// but continue scanning their children (don't skip the full size).
		if (tag === 0x03) {
			offset += 3; // ES_ID (2) + flags (1)
		} else if (tag === 0x04) {
			offset += 13; // objectTypeIndication (1) + streamType (1) + bufferSizeDB (3) + maxBitrate (4) + avgBitrate (4)
		} else {
			// Unknown tag — skip its payload entirely
			offset += size;
		}
	}

	return undefined;
}

/** `sample_is_non_sync_sample`, bit 16 of the ISO/IEC 14496-12 sample flags. */
const NON_SYNC = 0x0001_0000;

/** `sample_depends_on == 1`: the sample references others. */
const DEPENDS_ON_OTHERS = 0x0100_0000;

/**
 * Whether sample flags describe a sync sample: `sample_is_non_sync_sample` clear and not declared to
 * reference other samples, the rule ffmpeg applies and `moq-mux` mirrors. Flags that say nothing
 * (zero) describe a sync sample.
 */
function isSync(flags: number): boolean {
	return (flags & (NON_SYNC | DEPENDS_ON_OTHERS)) === 0;
}

/**
 * Parse a data segment (moof + mdat) to extract raw samples.
 *
 * Sample duration/size/flags fall back through trun → tfhd → trex (init segment)
 * per ISO/IEC 14496-12 §8.8.7. The init segment's trex defaults are required for
 * fragments where the encoder only set them once in moov (e.g. gstreamer passthrough).
 *
 * The moq-net `timestamp` is the broadcast timeline: the fragment's earliest sample presents at
 * it. `tfdt` and the composition offsets only place the samples relative to each other, since a
 * publisher may move a passthrough track to another timeline without rewriting the payload.
 * An untimed frame (`undefined`) has no broadcast time, so its samples present at the time
 * `tfdt` gives them.
 *
 * @param segment - The moof + mdat data
 * @param init - Parsed init segment (provides timescale and trex defaults)
 * @param timestamp - The moq-net frame timestamp carrying this segment, if its track is timed
 * @returns Array of decoded samples
 */
export function decodeDataSegment(
	segment: Uint8Array,
	init: InitSegment,
	timestamp: Time.Timestamp | undefined,
): Sample[] {
	// moof and mdat at the top level; the moof's first byte is where CMAF data offsets count from.
	const top = boxes(segment);
	const moof = required(top, "moof", "data segment");
	const mdat = required(top, "mdat", "data segment");

	const traf = boxes(required(boxes(moof.payload), "traf", "data segment").payload);

	// moof > traf > tfdt for the base media decode time.
	let baseDecodeTime = 0;
	const tfdtBox = child(traf, "tfdt");
	if (tfdtBox) {
		const tfdt = new Reader(tfdtBox.payload, "tfdt");
		baseDecodeTime = tfdt.full().version === 1 ? tfdt.u64() : tfdt.u32();
	}

	// moof > traf > tfhd for default sample values, falling back to trex from the init segment.
	let defaultDuration = init.defaultSampleDuration;
	let defaultSize = init.defaultSampleSize;
	let defaultFlags = init.defaultSampleFlags;
	const tfhdBox = child(traf, "tfhd");
	if (tfhdBox) {
		const tfhd = new Reader(tfhdBox.payload, "tfhd");
		const { flags } = tfhd.full();
		tfhd.skip(4); // track_ID
		// Samples are read from the mdat front to back, so each run must start where the previous
		// one ended. A run's dataOffset counts from the moof's first byte (CMAF's
		// default-base-is-moof).
		if (flags & 0x01)
			throw new Error("tfhd base_data_offset is unsupported: CMAF data offsets count from the moof");
		if (flags & 0x02) tfhd.skip(4); // sample_description_index
		if (flags & 0x08) defaultDuration = tfhd.u32();
		if (flags & 0x10) defaultSize = tfhd.u32();
		if (flags & 0x20) defaultFlags = tfhd.u32();
	}

	// moof > traf > trun for sample info. A traf may split its samples across several runs,
	// which continue one decode timeline and one mdat.
	const truns = traf.filter((box) => box.type === "trun");
	if (truns.length === 0) {
		throw new Error("No trun box found in data segment");
	}

	if (mdat.offset < moof.offset) {
		throw new Error("mdat must follow the moof");
	}
	const mdatData = mdat.payload;
	const dataStart = mdatData.byteOffset - segment.byteOffset - moof.offset;

	const samples: Sample[] = [];
	const ptss: number[] = [];

	let dataOffset = 0;
	let decodeTime = baseDecodeTime;

	for (const box of truns) {
		const trun = new Reader(box.payload, "trun");
		const { version, flags } = trun.full();
		const sampleCount = trun.u32();
		if (flags & 0x01) {
			const runOffset = trun.i32();
			if (runOffset !== dataStart + dataOffset) {
				throw new Error(
					`trun data_offset ${runOffset} doesn't start at the next sample (${dataStart + dataOffset})`,
				);
			}
		}
		const firstSampleFlags = flags & 0x04 ? trun.u32() : undefined;

		// Each sample's table entry is read as it is reached, never materialized up front, so the
		// count is checked against what backs it before any sample: a table must hold every entry
		// it declares, and a run sized by the defaults must fit in the mdat left.
		const entry =
			4 * ((flags & 0x100 ? 1 : 0) + (flags & 0x200 ? 1 : 0) + (flags & 0x400 ? 1 : 0) + (flags & 0x800 ? 1 : 0));
		if (entry * sampleCount > trun.remaining) {
			throw new Error(
				`trun declares ${sampleCount} samples but its table holds ${Math.floor(trun.remaining / entry)}`,
			);
		}
		if (!(flags & 0x200) && defaultSize > 0 && sampleCount * defaultSize > mdatData.length - dataOffset) {
			throw new Error(
				`trun declares ${sampleCount} samples of ${defaultSize} bytes, past the ${mdatData.length - dataOffset} mdat bytes left`,
			);
		}

		for (let i = 0; i < sampleCount; i++) {
			const sampleDuration = flags & 0x100 ? trun.u32() : defaultDuration;
			const sampleSize = flags & 0x200 ? trun.u32() : defaultSize;
			const entryFlags = flags & 0x400 ? trun.u32() : undefined;
			const compositionOffset = flags & 0x800 ? (version === 1 ? trun.i32() : trun.u32()) : 0;

			// Validate sample size - must be positive to produce valid data
			if (sampleSize <= 0) {
				throw new Error(`Invalid sample size ${sampleSize} for sample ${i} in trun`);
			}

			// Bounds check before slicing to prevent reading past mdat data
			if (dataOffset + sampleSize > mdatData.length) {
				throw new Error(
					`Sample ${i} would overflow mdat: offset=${dataOffset}, size=${sampleSize}, mdatLength=${mdatData.length}`,
				);
			}

			const sampleFlags =
				i === 0 && firstSampleFlags !== undefined ? firstSampleFlags : (entryFlags ?? defaultFlags);

			// Extract sample data
			const data = new Uint8Array(mdatData.slice(dataOffset, dataOffset + sampleSize));
			dataOffset += sampleSize;

			ptss.push(decodeTime + compositionOffset);
			const duration = Math.round((sampleDuration * 1_000_000) / init.timescale);

			// Audio never reports a keyframe: every audio sample is a sync sample, and the group
			// start is the consumer's to mark.
			const keyframe = init.kind === "video" && isSync(sampleFlags);

			// Set below, once the earliest presentation time is known.
			samples.push({ data, timestamp: 0, keyframe, duration });

			decodeTime += sampleDuration;
		}
	}

	// A loop, not `Math.min(...ptss)`: a long fragment's sample count can exceed the argument limit.
	let earliest = 0;
	if (timestamp !== undefined) {
		earliest = Number.POSITIVE_INFINITY;
		for (const pts of ptss) earliest = Math.min(earliest, pts);
	}
	const anchor = timestamp?.asMicros() ?? 0;
	for (const [i, sample] of samples.entries()) {
		sample.timestamp = Math.round(anchor + ((ptss[i] - earliest) * 1_000_000) / init.timescale);
	}

	return samples;
}
