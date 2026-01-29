/**
 * MP4 decoding utilities for parsing fMP4 init and data segments.
 * Used by WebCodecs to extract raw frames from CMAF container.
 */

import {
	type MediaHeaderBox,
	type ParsedIsoBox,
	readAvc1,
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
	readTrun,
	type SampleDescriptionBox,
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
	tkhd: readTkhd,
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
}

/**
 * A decoded sample from a data segment.
 */
export interface Sample {
	/** Raw sample data */
	data: Uint8Array;
	/** Timestamp in microseconds */
	timestamp: number;
	/** Whether this is a keyframe (sync sample) */
	keyframe: boolean;
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

	// Find moov > trak > mdia > minf > stbl > stsd for sample description
	const stsd = findBox(boxes, isBoxType<SampleDescriptionBox & ParsedIsoBox>("stsd"));
	if (!stsd || !stsd.entries || stsd.entries.length === 0) {
		throw new Error("No stsd box found in init segment");
	}

	// Extract codec-specific description from the first sample entry
	const entry = stsd.entries[0];
	const description = extractDescription(entry);

	return {
		description,
		timescale: mdhd.timescale,
		trackId,
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
				if (typeBytes === "avcC" || typeBytes === "hvcC" || typeBytes === "esds" || typeBytes === "dOps") {
					return new Uint8Array(box.slice(8));
				}
			}
			continue;
		}

		// Check for known codec config box types
		const boxType = box.type;
		if (boxType === "avcC" || boxType === "hvcC" || boxType === "esds" || boxType === "dOps") {
			// The library stores parsed boxes with a 'view' property containing IsoBoxReadView
			// which has access to the raw buffer. Extract the box payload (without header).
			if (box.view) {
				const view = box.view;
				// IsoBoxReadView has buffer, byteOffset, and byteLength properties
				// The box payload starts after the 8-byte header (size + type)
				const headerSize = 8;
				const payloadOffset = view.byteOffset + headerSize;
				const payloadLength = box.size - headerSize;
				return new Uint8Array(view.buffer, payloadOffset, payloadLength);
			}
			// Fallback: try data or raw properties
			if (box.data instanceof Uint8Array) {
				return new Uint8Array(box.data);
			}
			if (box.raw instanceof Uint8Array) {
				return new Uint8Array(box.raw.slice(8));
			}
		}
	}

	return undefined;
}

/**
 * Parse a data segment (moof + mdat) to extract raw samples.
 *
 * @param segment - The moof + mdat data
 * @param timescale - Time units per second (from init segment)
 * @returns Array of decoded samples
 */
export function decodeDataSegment(segment: Uint8Array, timescale: number): Sample[] {
	// Cast to ParsedIsoBox[] since the library's return type changes with readers
	const boxes = readIsoBoxes(toArrayBuffer(segment), { readers: DATA_READERS }) as ParsedIsoBox[];

	// Find moof > traf > tfdt for base media decode time
	const tfdt = findBox(boxes, isBoxType<TrackFragmentBaseMediaDecodeTimeBox & ParsedIsoBox>("tfdt"));
	const baseDecodeTime = tfdt?.baseMediaDecodeTime ?? 0;

	// Find moof > traf > tfhd for default sample values
	const tfhd = findBox(boxes, isBoxType<TrackFragmentHeaderBox & ParsedIsoBox>("tfhd"));
	const defaultDuration = tfhd?.defaultSampleDuration ?? 0;
	const defaultSize = tfhd?.defaultSampleSize ?? 0;
	const defaultFlags = tfhd?.defaultSampleFlags ?? 0;

	// Find moof > traf > trun for sample info
	const trun = findBox(boxes, isBoxType<TrackRunBox & ParsedIsoBox>("trun"));
	if (!trun) {
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

	const samples: Sample[] = [];

	// trun.dataOffset is an offset from the base data offset (typically moof start) to the first sample.
	// For simple CMAF segments where moof is followed immediately by mdat, this equals moof.size + 8.
	// Since mdat.data is the mdat payload (excluding the 8-byte header), we need to compute the
	// offset within mdatData. For now, we assume samples start at the beginning of mdat.data
	// when dataOffset is not specified or when it points to the start of mdat payload.
	// TODO: For complex cases with base_data_offset in tfhd, this needs additional handling.
	let dataOffset = 0;
	let decodeTime = baseDecodeTime;

	for (let i = 0; i < trun.sampleCount; i++) {
		const sample: TrackRunSample = trun.samples[i] ?? {};

		const sampleSize = sample.sampleSize ?? defaultSize;
		const sampleDuration = sample.sampleDuration ?? defaultDuration;

		// Validate sample size - must be positive to produce valid data
		if (sampleSize <= 0) {
			throw new Error(`Invalid sample size ${sampleSize} for sample ${i} in trun`);
		}

		// Validate sample duration - must be positive for proper timing
		if (sampleDuration <= 0) {
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

		// Calculate presentation timestamp in microseconds
		// PTS = (decode_time + composition_offset) * 1_000_000 / timescale
		const pts = decodeTime + compositionOffset;
		const timestamp = Math.round((pts * 1_000_000) / timescale);

		// Check if keyframe (sample_is_non_sync_sample flag is bit 16)
		// If flag is 0, treat as keyframe for safety
		const keyframe = sampleFlags === 0 || (sampleFlags & 0x00010000) === 0;

		samples.push({
			data,
			timestamp,
			keyframe,
		});

		decodeTime += sampleDuration;
	}

	return samples;
}

/**
 * Rewrite the track_id in a moof+mdat segment to 1.
 * This allows us to always generate init segments with track_id=1.
 *
 * Modifies the buffer in place and returns it.
 */
export function rewriteTrackId(segment: Uint8Array): Uint8Array {
	const tfhdOffset = findBoxOffset(segment, 0, segment.length, "tfhd");
	if (tfhdOffset === -1) {
		return segment; // No tfhd found, return as-is
	}

	// tfhd structure: size(4) + type(4) + version/flags(4) + track_id(4)
	// track_id is at offset 12 within the box
	const view = new DataView(segment.buffer, segment.byteOffset + tfhdOffset);
	view.setUint32(12, 1, false); // Set track_id to 1 (big-endian)

	return segment;
}

/**
 * Find the byte offset of a box by type within a range.
 * Recursively searches container boxes (moof, traf, etc.)
 */
function findBoxOffset(data: Uint8Array, start: number, end: number, targetType: string): number {
	const view = new DataView(data.buffer, data.byteOffset);
	let offset = start;

	while (offset + 8 <= end) {
		const size = view.getUint32(offset, false);
		if (size < 8 || offset + size > end) break;

		const type = String.fromCharCode(data[offset + 4], data[offset + 5], data[offset + 6], data[offset + 7]);

		if (type === targetType) {
			return offset;
		}

		// Recursively search container boxes
		if (type === "moof" || type === "traf" || type === "mdia" || type === "minf" || type === "stbl") {
			const found = findBoxOffset(data, offset + 8, offset + size, targetType);
			if (found !== -1) return found;
		}

		offset += size;
	}

	return -1;
}
