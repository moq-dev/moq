/**
 * MP4 decoding utilities for parsing fMP4 init and data segments.
 * Used by WebCodecs to extract raw frames from CMAF container.
 */

import type { Time } from "@moq/net";
import {
	type HandlerReferenceBox,
	type MediaHeaderBox,
	type ParsedIsoBox,
	readAvc1,
	readHdlr,
	readHev1,
	readHvc1,
	readIsoBoxes,
	readMdat,
	readMdhd,
	readMfhd,
	readMp4a,
	readStsd,
	readTfdt,
	readTfhd,
	readTkhd,
	readTrex,
	readTrun,
	type SampleDescriptionBox,
	type TrackExtendsBox,
	type TrackFragmentBaseMediaDecodeTimeBox,
	type TrackFragmentHeaderBox,
	type TrackRunBox,
	type TrackRunSample,
} from "@svta/cml-iso-bmff";

// Configure readers for specific box types we need to parse
const INIT_READERS = {
	avc1: readAvc1,
	avc3: readAvc1, // avc3 has same structure
	hvc1: readHvc1,
	hev1: readHev1,
	mp4a: readMp4a,
	stsd: readStsd,
	mdhd: readMdhd,
	hdlr: readHdlr,
	tkhd: readTkhd,
	trex: readTrex,
};

const DATA_READERS = {
	mfhd: readMfhd,
	tfhd: readTfhd,
	tfdt: readTfdt,
	trun: readTrun,
	mdat: readMdat,
};

/**
 * Recursively find a box by type in the box tree.
 * This is more reliable than the library's findIsoBox which may not traverse all children.
 */
function findBox<T extends ParsedIsoBox>(
	boxes: ParsedIsoBox[],
	predicate: (box: ParsedIsoBox) => box is T,
): T | undefined {
	for (const box of boxes) {
		if (predicate(box)) {
			return box;
		}
		// Recursively search children - boxes may have a 'boxes' property with children
		// biome-ignore lint/suspicious/noExplicitAny: ISO box structure varies
		const children = (box as any).boxes;
		if (children && Array.isArray(children)) {
			const found = findBox(children, predicate);
			if (found) return found;
		}
	}
	return undefined;
}

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

// Helper to convert Uint8Array to ArrayBuffer for the library
function toArrayBuffer(data: Uint8Array): ArrayBuffer {
	// Create a new ArrayBuffer and copy data to avoid SharedArrayBuffer issues
	const buffer = new ArrayBuffer(data.byteLength);
	new Uint8Array(buffer).set(data);
	return buffer;
}

// Type guard for finding boxes by type
function isBoxType<T extends ParsedIsoBox>(type: string) {
	return (box: ParsedIsoBox): box is T => box.type === type;
}

/**
 * Parse an init segment (ftyp + moov) to extract codec description and timescale.
 *
 * @param init - The init segment data
 * @returns Parsed init segment information
 */
export function decodeInitSegment(init: Uint8Array): InitSegment {
	// Cast to ParsedIsoBox[] since the library's return type changes with readers
	const boxes = readIsoBoxes(toArrayBuffer(init), { readers: INIT_READERS }) as ParsedIsoBox[];

	// Find moov > trak > mdia > mdhd for timescale
	const mdhd = findBox(boxes, isBoxType<MediaHeaderBox & ParsedIsoBox>("mdhd"));
	if (!mdhd) {
		throw new Error("No mdhd box found in init segment");
	}

	// Find moov > trak > tkhd for track ID
	// biome-ignore lint/suspicious/noExplicitAny: ISO box traversal
	const tkhd = findBox(boxes, isBoxType<any>("tkhd"));
	const trackId = tkhd?.trackId ?? 1;

	// Find moov > trak > mdia > hdlr for the track kind
	const hdlr = findBox(boxes, isBoxType<HandlerReferenceBox & ParsedIsoBox>("hdlr"));
	if (!hdlr) {
		throw new Error("No hdlr box found in init segment");
	}
	const kind = hdlr.handlerType === "soun" ? "audio" : hdlr.handlerType === "vide" ? "video" : undefined;
	if (!kind) {
		throw new Error(`Unsupported track handler: ${hdlr.handlerType}`);
	}

	// Find moov > trak > mdia > minf > stbl > stsd for sample description
	const stsd = findBox(boxes, isBoxType<SampleDescriptionBox & ParsedIsoBox>("stsd"));
	if (!stsd?.entries || stsd.entries.length === 0) {
		throw new Error("No stsd box found in init segment");
	}

	// Extract codec-specific description from the first sample entry
	const entry = stsd.entries[0];
	const description = extractDescription(entry);

	// Find moov > mvex > trex for this track to extract default sample values.
	// These are the bottom of the fallback chain when tfhd/trun don't specify them.
	const trex = findBox(
		boxes,
		(box): box is TrackExtendsBox & ParsedIsoBox =>
			box.type === "trex" && (box as TrackExtendsBox).trackId === trackId,
	);

	return {
		description,
		timescale: mdhd.timescale,
		trackId,
		kind,
		defaultSampleDuration: trex?.defaultSampleDuration ?? 0,
		defaultSampleSize: trex?.defaultSampleSize ?? 0,
		defaultSampleFlags: trex?.defaultSampleFlags ?? 0,
	};
}

/**
 * Extract codec-specific description from a sample entry.
 * The description is codec-specific: avcC for H.264, hvcC for H.265, esds for AAC, dOps for Opus.
 */
// biome-ignore lint/suspicious/noExplicitAny: ISO box types vary
function extractDescription(entry: any): Uint8Array | undefined {
	if (!entry.boxes || !Array.isArray(entry.boxes)) {
		return undefined;
	}

	// Look for codec config boxes in the sample entry
	for (const box of entry.boxes) {
		// Handle raw Uint8Array boxes (already serialized)
		if (box instanceof Uint8Array) {
			// Extract the payload without the box header (8 bytes: 4 size + 4 type)
			if (box.length > 8) {
				// Check if this looks like a codec config box by reading the type
				const typeBytes = String.fromCharCode(box[4], box[5], box[6], box[7]);
				if (typeBytes === "avcC" || typeBytes === "hvcC" || typeBytes === "dOps") {
					return new Uint8Array(box.slice(8));
				}
				if (typeBytes === "esds") {
					// esds payload has nested descriptors; extract the AudioSpecificConfig (tag 0x05).
					return extractAudioSpecificConfig(new Uint8Array(box.slice(8)));
				}
			}
			continue;
		}

		// Check for known codec config box types
		const boxType = box.type;
		if (boxType === "avcC" || boxType === "hvcC" || boxType === "dOps") {
			if (box.view) {
				const view = box.view;
				const headerSize = 8;
				const payloadOffset = view.byteOffset + headerSize;
				const payloadLength = box.size - headerSize;
				return new Uint8Array(view.buffer, payloadOffset, payloadLength);
			}
			if (box.data instanceof Uint8Array) {
				return new Uint8Array(box.data);
			}
			if (box.raw instanceof Uint8Array) {
				return new Uint8Array(box.raw.slice(8));
			}
		}
		if (boxType === "esds") {
			let payload: Uint8Array | undefined;
			if (box.view) {
				const view = box.view;
				const headerSize = 8;
				payload = new Uint8Array(view.buffer, view.byteOffset + headerSize, box.size - headerSize);
			} else if (box.data instanceof Uint8Array) {
				payload = new Uint8Array(box.data);
			} else if (box.raw instanceof Uint8Array) {
				payload = new Uint8Array(box.raw.slice(8));
			}
			if (payload) return extractAudioSpecificConfig(payload);
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
	// Cast to ParsedIsoBox[] since the library's return type changes with readers
	const boxes = readIsoBoxes(toArrayBuffer(segment), { readers: DATA_READERS }) as ParsedIsoBox[];

	// Find moof > traf > tfdt for base media decode time
	const tfdt = findBox(boxes, isBoxType<TrackFragmentBaseMediaDecodeTimeBox & ParsedIsoBox>("tfdt"));
	const baseDecodeTime = tfdt?.baseMediaDecodeTime ?? 0;

	// Find moof > traf > tfhd for default sample values, falling back to trex from the init segment.
	const tfhd = findBox(boxes, isBoxType<TrackFragmentHeaderBox & ParsedIsoBox>("tfhd"));
	const defaultDuration = tfhd?.defaultSampleDuration ?? init.defaultSampleDuration;
	const defaultSize = tfhd?.defaultSampleSize ?? init.defaultSampleSize;
	const defaultFlags = tfhd?.defaultSampleFlags ?? init.defaultSampleFlags;

	// Find moof > traf > trun for sample info. A traf may split its samples across several runs,
	// which continue one decode timeline and one mdat.
	const traf = findBox(boxes, isBoxType<ParsedIsoBox>("traf"));
	// biome-ignore lint/suspicious/noExplicitAny: ISO box structure varies
	const truns = ((traf as any)?.boxes ?? []).filter(isBoxType<TrackRunBox & ParsedIsoBox>("trun"));
	if (truns.length === 0) {
		throw new Error("No trun box found in data segment");
	}

	// Find mdat for sample data
	// biome-ignore lint/suspicious/noExplicitAny: mdat box type
	const mdat = findBox(boxes, isBoxType<any>("mdat"));
	if (!mdat) {
		throw new Error("No mdat box found in data segment");
	}

	// mdat.data contains the raw sample data
	const mdatData = mdat.data as Uint8Array;
	if (!mdatData) {
		throw new Error("No data in mdat box");
	}

	// Samples are read from the mdat front to back, so each run must start where the previous one
	// ended. A run's dataOffset counts from the moof's first byte (CMAF's default-base-is-moof).
	if (tfhd?.baseDataOffset !== undefined) {
		throw new Error("tfhd base_data_offset is unsupported: CMAF data offsets count from the moof");
	}
	let position = 0;
	let moofStart: number | undefined;
	let mdatDataStart: number | undefined;
	for (const box of boxes) {
		const size = box.largesize ?? box.size;
		if (box.type === "moof") moofStart ??= position;
		if (box === mdat) mdatDataStart = position + size - mdatData.byteLength;
		position += size;
	}
	if (moofStart === undefined || mdatDataStart === undefined || mdatDataStart < moofStart) {
		throw new Error("mdat must follow the moof");
	}
	const dataStart = mdatDataStart - moofStart;

	const samples: Sample[] = [];
	const ptss: number[] = [];

	let dataOffset = 0;
	let decodeTime = baseDecodeTime;

	for (const trun of truns) {
		if (trun.dataOffset !== undefined && trun.dataOffset !== dataStart + dataOffset) {
			throw new Error(
				`trun data_offset ${trun.dataOffset} doesn't start at the next sample (${dataStart + dataOffset})`,
			);
		}

		for (let i = 0; i < trun.sampleCount; i++) {
			const sample: TrackRunSample = trun.samples[i] ?? {};

			const sampleSize = sample.sampleSize ?? defaultSize;
			const sampleDuration = sample.sampleDuration ?? defaultDuration;

			// Validate sample size - must be positive to produce valid data
			if (sampleSize <= 0) {
				throw new Error(`Invalid sample size ${sampleSize} for sample ${i} in trun`);
			}

			// Duration 0 is valid for single-sample CMAF fragments where duration
			// is implicit. Negative duration would indicate corrupt data.
			if (sampleDuration < 0) {
				throw new Error(`Invalid sample duration ${sampleDuration} for sample ${i} in trun`);
			}

			// Bounds check before slicing to prevent reading past mdat data
			if (dataOffset + sampleSize > mdatData.length) {
				throw new Error(
					`Sample ${i} would overflow mdat: offset=${dataOffset}, size=${sampleSize}, mdatLength=${mdatData.length}`,
				);
			}

			const sampleFlags =
				i === 0 && trun.firstSampleFlags !== undefined
					? trun.firstSampleFlags
					: (sample.sampleFlags ?? defaultFlags);
			const compositionOffset = sample.sampleCompositionTimeOffset ?? 0;

			// Extract sample data
			const data = new Uint8Array(mdatData.slice(dataOffset, dataOffset + sampleSize));
			dataOffset += sampleSize;

			ptss.push(decodeTime + compositionOffset);
			const duration = Math.round((sampleDuration * 1_000_000) / init.timescale);

			// Check if keyframe (sample_is_non_sync_sample flag is bit 16)
			// If flag is 0, treat as keyframe for safety. Audio never reports one: every
			// audio sample is a sync sample, and the group start is the consumer's to mark.
			const keyframe = init.kind === "video" && (sampleFlags === 0 || (sampleFlags & 0x00010000) === 0);

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
