/**
 * MP4 encoding utilities for creating fMP4 init and data segments.
 * Used by MSE to create init segments (ftyp+moov) and data segments (moof+mdat).
 */

import {
	type DataEntryUrlBox,
	type DataInformationBox,
	type DataReferenceBox,
	type DecodingTimeToSampleBox,
	type FileTypeBox,
	type HandlerReferenceBox,
	type IsoBoxStreamable,
	type IsoBoxWriterMap,
	type MediaBox,
	type MediaDataBox,
	type MediaHeaderBox,
	type MediaInformationBox,
	type MovieBox,
	type MovieExtendsBox,
	type MovieFragmentBox,
	type MovieFragmentHeaderBox,
	type MovieHeaderBox,
	type SampleDescriptionBox,
	type SampleTableBox,
	type SoundMediaHeaderBox,
	type TrackBox,
	type TrackExtendsBox,
	type TrackFragmentBaseMediaDecodeTimeBox,
	type TrackFragmentBox,
	type TrackFragmentHeaderBox,
	type TrackHeaderBox,
	type TrackRunBox,
	type VideoMediaHeaderBox,
	writeDref,
	writeFtyp,
	writeHdlr,
	writeIsoBoxes,
	writeMdat,
	writeMdhd,
	writeMfhd,
	writeMvhd,
	writeSmhd,
	writeStsd,
	writeStts,
	writeTfdt,
	writeTfhd,
	writeTkhd,
	writeTrex,
	writeTrun,
	writeUrl,
	writeVmhd,
} from "@svta/cml-iso-bmff";

import type * as Catalog from "../catalog";
import * as Hex from "../util/hex";

// Identity matrix for tkhd/mvhd (stored as 16.16 fixed point)
const IDENTITY_MATRIX = [0x00010000, 0, 0, 0, 0x00010000, 0, 0, 0, 0x40000000];

// Writers config - maps box types to their writer functions
const WRITERS: IsoBoxWriterMap = {
	// Init segment boxes
	ftyp: writeFtyp,
	mvhd: writeMvhd,
	tkhd: writeTkhd,
	mdhd: writeMdhd,
	hdlr: writeHdlr,
	vmhd: writeVmhd,
	smhd: writeSmhd,
	"url ": writeUrl,
	dref: writeDref,
	stsd: writeStsd,
	stts: writeStts,
	trex: writeTrex,
	// Data segment boxes
	mfhd: writeMfhd,
	tfhd: writeTfhd,
	tfdt: writeTfdt,
	trun: writeTrun,
	mdat: writeMdat,
	// For boxes without library writers, we create them manually as Uint8Arrays
};

/** Write boxes using our WRITERS config. */
function writeBoxes(boxes: Iterable<IsoBoxStreamable>): Uint8Array[] {
	return writeIsoBoxes(boxes, { writers: WRITERS });
}

/**
 * Helper to create a simple full box (version + flags + content) as raw bytes.
 * Used for boxes that don't have writers in the library.
 */
function createFullBox(type: string, version: number, flags: number, content: Uint8Array): Uint8Array {
	const size = 8 + 4 + content.length; // header + version/flags + content
	const box = new Uint8Array(size);
	const view = new DataView(box.buffer);

	view.setUint32(0, size, false);
	box[4] = type.charCodeAt(0);
	box[5] = type.charCodeAt(1);
	box[6] = type.charCodeAt(2);
	box[7] = type.charCodeAt(3);
	view.setUint32(8, (version << 24) | flags, false);
	box.set(content, 12);

	return box;
}

/**
 * Create an empty stsc (Sample to Chunk) box for fragmented MP4.
 */
function createEmptyStsc(): Uint8Array {
	// stsc: version(1) + flags(3) + entry_count(4) = 4 bytes content
	const content = new Uint8Array(4); // entry_count = 0
	return createFullBox("stsc", 0, 0, content);
}

/**
 * Create an empty stsz (Sample Size) box for fragmented MP4.
 */
function createEmptyStsz(): Uint8Array {
	// stsz: version(1) + flags(3) + sample_size(4) + sample_count(4) = 8 bytes content
	const content = new Uint8Array(8); // sample_size = 0, sample_count = 0
	return createFullBox("stsz", 0, 0, content);
}

/**
 * Create an empty stco (Chunk Offset) box for fragmented MP4.
 */
function createEmptyStco(): Uint8Array {
	// stco: version(1) + flags(3) + entry_count(4) = 4 bytes content
	const content = new Uint8Array(4); // entry_count = 0
	return createFullBox("stco", 0, 0, content);
}

/**
 * Create an avc1 (H.264 Visual Sample Entry) box with embedded avcC.
 * Built manually because the library doesn't properly serialize Uint8Array child boxes.
 */
function createAvc1Box(width: number, height: number, avcC: Uint8Array): Uint8Array {
	// avc1 box structure:
	// - 6 bytes reserved (0)
	// - 2 bytes data_reference_index (1)
	// - 2 bytes pre_defined (0)
	// - 2 bytes reserved (0)
	// - 12 bytes pre_defined (0)
	// - 2 bytes width
	// - 2 bytes height
	// - 4 bytes horizresolution (0x00480000 = 72 dpi)
	// - 4 bytes vertresolution (0x00480000 = 72 dpi)
	// - 4 bytes reserved (0)
	// - 2 bytes frame_count (1)
	// - 32 bytes compressorname (null-padded string)
	// - 2 bytes depth (0x0018 = 24)
	// - 2 bytes pre_defined (-1 = 0xFFFF)
	// - child boxes (avcC)

	const avcCSize = 8 + avcC.length; // box header + payload
	const avc1ContentSize = 6 + 2 + 2 + 2 + 12 + 2 + 2 + 4 + 4 + 4 + 2 + 32 + 2 + 2 + avcCSize;
	const avc1Size = 8 + avc1ContentSize; // box header + content

	const box = new Uint8Array(avc1Size);
	const view = new DataView(box.buffer);
	let offset = 0;

	// Box header
	view.setUint32(offset, avc1Size, false);
	offset += 4;
	box[offset++] = 0x61; // 'a'
	box[offset++] = 0x76; // 'v'
	box[offset++] = 0x63; // 'c'
	box[offset++] = 0x31; // '1'

	// SampleEntry fields
	offset += 6; // reserved (6 bytes of 0)
	view.setUint16(offset, 1, false);
	offset += 2; // data_reference_index = 1

	// VisualSampleEntry fields
	view.setUint16(offset, 0, false);
	offset += 2; // pre_defined
	view.setUint16(offset, 0, false);
	offset += 2; // reserved
	offset += 12; // pre_defined (12 bytes of 0)
	view.setUint16(offset, width, false);
	offset += 2;
	view.setUint16(offset, height, false);
	offset += 2;
	view.setUint32(offset, 0x00480000, false);
	offset += 4; // horizresolution (72 dpi)
	view.setUint32(offset, 0x00480000, false);
	offset += 4; // vertresolution (72 dpi)
	view.setUint32(offset, 0, false);
	offset += 4; // reserved
	view.setUint16(offset, 1, false);
	offset += 2; // frame_count = 1
	offset += 32; // compressorname (32 bytes of 0)
	view.setUint16(offset, 0x0018, false);
	offset += 2; // depth = 24
	view.setUint16(offset, 0xffff, false);
	offset += 2; // pre_defined = -1

	// avcC child box
	view.setUint32(offset, avcCSize, false);
	offset += 4;
	box[offset++] = 0x61; // 'a'
	box[offset++] = 0x76; // 'v'
	box[offset++] = 0x63; // 'c'
	box[offset++] = 0x43; // 'C'
	box.set(avcC, offset);

	return box;
}

/**
 * Creates an MSE-compatible initialization segment (ftyp + moov) for H.264 video.
 *
 * @example
 * ```ts
 * // From WebCodecs EncodedVideoChunkMetadata
 * const config = await encoder.encode(frame);
 * const metadata = config.decoderConfig;
 *
 * const initSegment = createVideoInitSegment({
 *   width: metadata.codedWidth,
 *   height: metadata.codedHeight,
 *   avcC: new Uint8Array(metadata.description),
 * });
 *
 * sourceBuffer.appendBuffer(initSegment);
 * ```
 */
export function createVideoInitSegment(config: Catalog.VideoConfig): Uint8Array {
	const { codedWidth, codedHeight, description } = config;
	if (!codedWidth || !codedHeight || !description) {
		// TODO: We could
		throw new Error("Missing required fields to create video init segment");
	}

	const timescale = 1_000_000; // microseconds

	// Always use track_id=1. Incoming CMAF data segments are rewritten to match.
	const trackId = 1;

	// ftyp - File Type Box
	const ftyp: FileTypeBox = {
		type: "ftyp",
		majorBrand: "isom",
		minorVersion: 0x200,
		compatibleBrands: ["isom", "iso6", "mp41"],
	};

	// mvhd - Movie Header Box
	const mvhd: MovieHeaderBox = {
		type: "mvhd",
		version: 0,
		flags: 0,
		creationTime: 0,
		modificationTime: 0,
		timescale: timescale,
		duration: 0, // Unknown/fragmented
		rate: 0x00010000, // 1.0 in 16.16 fixed point
		volume: 0x0100, // 1.0 in 8.8 fixed point
		reserved1: 0,
		reserved2: [0, 0],
		matrix: IDENTITY_MATRIX,
		preDefined: [0, 0, 0, 0, 0, 0],
		nextTrackId: trackId + 1,
	};

	// tkhd - Track Header Box
	const tkhd: TrackHeaderBox = {
		type: "tkhd",
		version: 0,
		flags: 0x000003, // Track enabled + in movie
		creationTime: 0,
		modificationTime: 0,
		trackId: trackId,
		reserved1: 0,
		duration: 0,
		reserved2: [0, 0],
		layer: 0,
		alternateGroup: 0,
		volume: 0, // Video tracks have 0 volume
		reserved3: 0,
		matrix: IDENTITY_MATRIX,
		width: codedWidth * 0x10000, // 16.16 fixed point (avoid << which produces signed int)
		height: codedHeight * 0x10000,
	};

	// mdhd - Media Header Box
	const mdhd: MediaHeaderBox = {
		type: "mdhd",
		version: 0,
		flags: 0,
		creationTime: 0,
		modificationTime: 0,
		timescale: timescale,
		duration: 0,
		language: "und",
		preDefined: 0,
	};

	// hdlr - Handler Reference Box
	const hdlr: HandlerReferenceBox = {
		type: "hdlr",
		version: 0,
		flags: 0,
		preDefined: 0,
		handlerType: "vide",
		reserved: [0, 0, 0],
		name: "VideoHandler",
	};

	// vmhd - Video Media Header Box
	const vmhd: VideoMediaHeaderBox = {
		type: "vmhd",
		version: 0,
		flags: 1, // Required to be 1
		graphicsmode: 0,
		opcolor: [0, 0, 0],
	};

	// url - Data Entry URL Box (self-contained)
	const urlBox: DataEntryUrlBox = {
		type: "url ",
		version: 0,
		flags: 0x000001, // Self-contained flag
		location: "",
	};

	// dref - Data Reference Box
	const dref: DataReferenceBox = {
		type: "dref",
		version: 0,
		flags: 0,
		entryCount: 1,
		entries: [urlBox],
	};

	// dinf - Data Information Box
	const dinf: DataInformationBox = {
		type: "dinf",
		boxes: [dref],
	};

	// Build the avc1 box manually since the library doesn't properly serialize Uint8Array children
	const avc1Box = createAvc1Box(codedWidth, codedHeight, Hex.toBytes(description));

	// stsd - Sample Description Box
	const stsd: SampleDescriptionBox = {
		type: "stsd",
		version: 0,
		flags: 0,
		entryCount: 1,
		// biome-ignore lint/suspicious/noExplicitAny: Raw avc1 box since library doesn't handle avcC children
		entries: [avc1Box] as any[],
	};

	// stts - Decoding Time to Sample (empty for fragmented)
	const stts: DecodingTimeToSampleBox = {
		type: "stts",
		version: 0,
		flags: 0,
		entryCount: 0,
		entries: [],
	};

	// Create raw boxes for types without library writers
	const stsc = createEmptyStsc();
	const stsz = createEmptyStsz();
	const stco = createEmptyStco();

	// stbl - Sample Table Box
	// Note: stsc, stsz, stco are raw Uint8Arrays since the library doesn't have writers for them
	const stbl: SampleTableBox = {
		type: "stbl",
		// biome-ignore lint/suspicious/noExplicitAny: Raw boxes for types without library writers
		boxes: [stsd, stts, stsc, stsz, stco] as any[],
	};

	// minf - Media Information Box
	const minf: MediaInformationBox = {
		type: "minf",
		boxes: [vmhd, dinf, stbl],
	};

	// mdia - Media Box
	const mdia: MediaBox = {
		type: "mdia",
		boxes: [mdhd, hdlr, minf],
	};

	// trak - Track Box
	const trak: TrackBox = {
		type: "trak",
		boxes: [tkhd, mdia],
	};

	// trex - Track Extends Box (required for fragmented MP4)
	const trex: TrackExtendsBox = {
		type: "trex",
		version: 0,
		flags: 0,
		trackId: trackId,
		defaultSampleDescriptionIndex: 1,
		defaultSampleDuration: 0,
		defaultSampleSize: 0,
		defaultSampleFlags: 0,
	};

	// mvex - Movie Extends Box (signals fragmented MP4)
	const mvex: MovieExtendsBox = {
		type: "mvex",
		boxes: [trex],
	};

	// moov - Movie Box
	const moov: MovieBox = {
		type: "moov",
		boxes: [mvhd, trak, mvex],
	};

	// Write all boxes and concatenate
	const buffers = writeBoxes([ftyp, moov]);
	const totalLength = buffers.reduce((sum, buf) => sum + buf.byteLength, 0);
	const result = new Uint8Array(totalLength);

	let offset = 0;
	for (const buf of buffers) {
		result.set(new Uint8Array(buf.buffer, buf.byteOffset, buf.byteLength), offset);
		offset += buf.byteLength;
	}

	return result;
}

/**
 * Creates an MSE-compatible initialization segment (ftyp + moov) for audio.
 * Supports AAC (mp4a) and Opus codecs.
 */
export function createAudioInitSegment(config: Catalog.AudioConfig): Uint8Array {
	const { sampleRate, numberOfChannels, description, codec } = config;

	const timescale = 1_000_000; // microseconds
	const trackId = 1;

	// ftyp - File Type Box
	const ftyp: FileTypeBox = {
		type: "ftyp",
		majorBrand: "isom",
		minorVersion: 0x200,
		compatibleBrands: ["isom", "iso6", "mp41"],
	};

	// mvhd - Movie Header Box
	const mvhd: MovieHeaderBox = {
		type: "mvhd",
		version: 0,
		flags: 0,
		creationTime: 0,
		modificationTime: 0,
		timescale: timescale,
		duration: 0,
		rate: 0x00010000,
		volume: 0x0100,
		reserved1: 0,
		reserved2: [0, 0],
		matrix: IDENTITY_MATRIX,
		preDefined: [0, 0, 0, 0, 0, 0],
		nextTrackId: trackId + 1,
	};

	// tkhd - Track Header Box
	const tkhd: TrackHeaderBox = {
		type: "tkhd",
		version: 0,
		flags: 0x000003,
		creationTime: 0,
		modificationTime: 0,
		trackId: trackId,
		reserved1: 0,
		duration: 0,
		reserved2: [0, 0],
		layer: 0,
		alternateGroup: 0,
		volume: 0x0100, // Audio tracks have volume (1.0 in 8.8 fixed point)
		reserved3: 0,
		matrix: IDENTITY_MATRIX,
		width: 0,
		height: 0,
	};

	// mdhd - Media Header Box
	const mdhd: MediaHeaderBox = {
		type: "mdhd",
		version: 0,
		flags: 0,
		creationTime: 0,
		modificationTime: 0,
		timescale: timescale,
		duration: 0,
		language: "und",
		preDefined: 0,
	};

	// hdlr - Handler Reference Box
	const hdlr: HandlerReferenceBox = {
		type: "hdlr",
		version: 0,
		flags: 0,
		preDefined: 0,
		handlerType: "soun",
		reserved: [0, 0, 0],
		name: "SoundHandler",
	};

	// smhd - Sound Media Header Box
	const smhd: SoundMediaHeaderBox = {
		type: "smhd",
		version: 0,
		flags: 0,
		balance: 0,
		reserved: 0,
	};

	// url - Data Entry URL Box (self-contained)
	const urlBox: DataEntryUrlBox = {
		type: "url ",
		version: 0,
		flags: 0x000001,
		location: "",
	};

	// dref - Data Reference Box
	const dref: DataReferenceBox = {
		type: "dref",
		version: 0,
		flags: 0,
		entryCount: 1,
		entries: [urlBox],
	};

	// dinf - Data Information Box
	const dinf: DataInformationBox = {
		type: "dinf",
		boxes: [dref],
	};

	// Build codec-specific sample entry (manually to ensure child boxes are properly serialized)
	const sampleEntry = createAudioSampleEntry(codec, sampleRate, numberOfChannels, description);

	// stsd - Sample Description Box
	const stsd: SampleDescriptionBox = {
		type: "stsd",
		version: 0,
		flags: 0,
		entryCount: 1,
		// biome-ignore lint/suspicious/noExplicitAny: Raw sample entry box
		entries: [sampleEntry] as any[],
	};

	// stts - Decoding Time to Sample (empty for fragmented)
	const stts: DecodingTimeToSampleBox = {
		type: "stts",
		version: 0,
		flags: 0,
		entryCount: 0,
		entries: [],
	};

	// Create raw boxes for types without library writers
	const stsc = createEmptyStsc();
	const stsz = createEmptyStsz();
	const stco = createEmptyStco();

	// stbl - Sample Table Box
	// Note: stsc, stsz, stco are raw Uint8Arrays since the library doesn't have writers for them
	const stbl: SampleTableBox = {
		type: "stbl",
		// biome-ignore lint/suspicious/noExplicitAny: Raw boxes for types without library writers
		boxes: [stsd, stts, stsc, stsz, stco] as any[],
	};

	// minf - Media Information Box
	const minf: MediaInformationBox = {
		type: "minf",
		boxes: [smhd, dinf, stbl],
	};

	// mdia - Media Box
	const mdia: MediaBox = {
		type: "mdia",
		boxes: [mdhd, hdlr, minf],
	};

	// trak - Track Box
	const trak: TrackBox = {
		type: "trak",
		boxes: [tkhd, mdia],
	};

	// trex - Track Extends Box
	const trex: TrackExtendsBox = {
		type: "trex",
		version: 0,
		flags: 0,
		trackId: trackId,
		defaultSampleDescriptionIndex: 1,
		defaultSampleDuration: 0,
		defaultSampleSize: 0,
		defaultSampleFlags: 0,
	};

	// mvex - Movie Extends Box
	const mvex: MovieExtendsBox = {
		type: "mvex",
		boxes: [trex],
	};

	// moov - Movie Box
	const moov: MovieBox = {
		type: "moov",
		boxes: [mvhd, trak, mvex],
	};

	// Write all boxes and concatenate
	const buffers = writeBoxes([ftyp, moov]);
	const totalLength = buffers.reduce((sum, buf) => sum + buf.byteLength, 0);
	const result = new Uint8Array(totalLength);

	let offset = 0;
	for (const buf of buffers) {
		result.set(new Uint8Array(buf.buffer, buf.byteOffset, buf.byteLength), offset);
		offset += buf.byteLength;
	}

	return result;
}

/**
 * Create an audio sample entry box (mp4a or Opus) with embedded codec config.
 * Built manually because the library doesn't properly serialize Uint8Array child boxes.
 */
function createAudioSampleEntry(
	codec: string,
	sampleRate: number,
	channelCount: number,
	description?: string,
): Uint8Array {
	if (codec.startsWith("mp4a")) {
		return createMp4aBox(sampleRate, channelCount, description);
	} else if (codec === "opus") {
		return createOpusBox(sampleRate, channelCount, description);
	}
	throw new Error(`Unsupported audio codec: ${codec}`);
}

/**
 * Create an mp4a (AAC Audio Sample Entry) box with embedded esds.
 */
function createMp4aBox(sampleRate: number, channelCount: number, description?: string): Uint8Array {
	const esds = createEsdsBox(sampleRate, channelCount, description);

	// mp4a box structure (AudioSampleEntry):
	// - 6 bytes reserved (0)
	// - 2 bytes data_reference_index (1)
	// - 8 bytes reserved (0) - includes reserved2[2] and pre_defined fields
	// - 2 bytes channelcount
	// - 2 bytes samplesize (16)
	// - 2 bytes pre_defined (0)
	// - 2 bytes reserved (0)
	// - 4 bytes samplerate (16.16 fixed point)
	// - child boxes (esds)

	const mp4aContentSize = 6 + 2 + 8 + 2 + 2 + 2 + 2 + 4 + esds.length;
	const mp4aSize = 8 + mp4aContentSize;

	const box = new Uint8Array(mp4aSize);
	const view = new DataView(box.buffer);
	let offset = 0;

	// Box header
	view.setUint32(offset, mp4aSize, false);
	offset += 4;
	box[offset++] = 0x6d; // 'm'
	box[offset++] = 0x70; // 'p'
	box[offset++] = 0x34; // '4'
	box[offset++] = 0x61; // 'a'

	// SampleEntry fields
	offset += 6; // reserved (6 bytes of 0)
	view.setUint16(offset, 1, false);
	offset += 2; // data_reference_index = 1

	// AudioSampleEntry fields
	offset += 8; // reserved (8 bytes of 0)
	view.setUint16(offset, channelCount, false);
	offset += 2;
	view.setUint16(offset, 16, false);
	offset += 2; // samplesize = 16
	view.setUint16(offset, 0, false);
	offset += 2; // pre_defined
	view.setUint16(offset, 0, false);
	offset += 2; // reserved
	view.setUint32(offset, sampleRate * 0x10000, false);
	offset += 4; // samplerate (16.16 fixed point)

	// esds child box (already includes box header)
	box.set(esds, offset);

	return box;
}

/**
 * Create an Opus (Opus Audio Sample Entry) box with embedded dOps.
 */
function createOpusBox(sampleRate: number, channelCount: number, description?: string): Uint8Array {
	const dOps = createDOpsBox(channelCount, sampleRate, description);

	// Opus box structure (AudioSampleEntry):
	// Same structure as mp4a
	const opusContentSize = 6 + 2 + 8 + 2 + 2 + 2 + 2 + 4 + dOps.length;
	const opusSize = 8 + opusContentSize;

	const box = new Uint8Array(opusSize);
	const view = new DataView(box.buffer);
	let offset = 0;

	// Box header
	view.setUint32(offset, opusSize, false);
	offset += 4;
	box[offset++] = 0x4f; // 'O'
	box[offset++] = 0x70; // 'p'
	box[offset++] = 0x75; // 'u'
	box[offset++] = 0x73; // 's'

	// SampleEntry fields
	offset += 6; // reserved (6 bytes of 0)
	view.setUint16(offset, 1, false);
	offset += 2; // data_reference_index = 1

	// AudioSampleEntry fields
	offset += 8; // reserved (8 bytes of 0)
	view.setUint16(offset, channelCount, false);
	offset += 2;
	view.setUint16(offset, 16, false);
	offset += 2; // samplesize = 16
	view.setUint16(offset, 0, false);
	offset += 2; // pre_defined
	view.setUint16(offset, 0, false);
	offset += 2; // reserved
	view.setUint32(offset, sampleRate * 0x10000, false);
	offset += 4; // samplerate (16.16 fixed point)

	// dOps child box (already includes box header)
	box.set(dOps, offset);

	return box;
}

/**
 * Generate an AudioSpecificConfig for AAC-LC.
 * Format: 5 bits audioObjectType + 4 bits samplingFrequencyIndex + 4 bits channelConfiguration + 3 bits (zeros)
 */
function generateAudioSpecificConfig(sampleRate: number, channelCount: number): Uint8Array {
	// Sampling frequency index lookup
	const sampleRateIndex: Record<number, number> = {
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

	const freqIndex = sampleRateIndex[sampleRate] ?? 4; // Default to 44100 if not found
	const audioObjectType = 2; // AAC-LC

	// AudioSpecificConfig is 2 bytes for AAC-LC:
	// 5 bits: audioObjectType (2 for AAC-LC)
	// 4 bits: samplingFrequencyIndex
	// 4 bits: channelConfiguration
	// 3 bits: GASpecificConfig (frameLengthFlag=0, dependsOnCoreCoder=0, extensionFlag=0)
	const byte0 = (audioObjectType << 3) | (freqIndex >> 1);
	const byte1 = ((freqIndex & 1) << 7) | (channelCount << 3);

	return new Uint8Array([byte0, byte1]);
}

/**
 * Creates an esds (Elementary Stream Descriptor) box for AAC.
 * The description from WebCodecs is the AudioSpecificConfig.
 * If no description is provided, generates one from sampleRate and channelCount.
 */
function createEsdsBox(sampleRate: number, channelCount: number, description?: string): Uint8Array {
	const audioSpecificConfig = description
		? Hex.toBytes(description)
		: generateAudioSpecificConfig(sampleRate, channelCount);

	// ES_Descriptor structure:
	// - tag (0x03) + size + ES_ID (2) + flags (1)
	// - DecoderConfigDescriptor: tag (0x04) + size + objectTypeIndication (1) + streamType (1) + bufferSizeDB (3) + maxBitrate (4) + avgBitrate (4)
	//   - DecoderSpecificInfo: tag (0x05) + size + AudioSpecificConfig
	// - SLConfigDescriptor: tag (0x06) + size + predefined (1)

	const decSpecificInfoSize = audioSpecificConfig.length;
	const decConfigDescSize = 13 + 2 + decSpecificInfoSize; // 13 fixed + tag/size + ASC
	const esDescSize = 3 + 2 + decConfigDescSize + 3; // 3 fixed + tag/size + DCD + SLC (3 bytes)

	const esdsSize = 12 + 2 + esDescSize; // 4 (size) + 4 (type) + 4 (version/flags) + tag/size + ESD
	const esds = new Uint8Array(esdsSize);
	const view = new DataView(esds.buffer);

	let offset = 0;

	// Box header
	view.setUint32(offset, esdsSize, false);
	offset += 4;
	esds[offset++] = 0x65; // 'e'
	esds[offset++] = 0x73; // 's'
	esds[offset++] = 0x64; // 'd'
	esds[offset++] = 0x73; // 's'

	// Version and flags (full box)
	view.setUint32(offset, 0, false);
	offset += 4;

	// ES_Descriptor
	esds[offset++] = 0x03; // tag
	esds[offset++] = esDescSize; // size (assuming < 128)

	view.setUint16(offset, 0, false);
	offset += 2; // ES_ID
	esds[offset++] = 0; // flags

	// DecoderConfigDescriptor
	esds[offset++] = 0x04; // tag
	esds[offset++] = decConfigDescSize; // size

	esds[offset++] = 0x40; // objectTypeIndication: Audio ISO/IEC 14496-3 (AAC)
	esds[offset++] = 0x15; // streamType (5 = audio) << 2 | upstream (0) << 1 | reserved (1)
	esds[offset++] = 0x00; // bufferSizeDB (3 bytes)
	esds[offset++] = 0x00;
	esds[offset++] = 0x00;
	view.setUint32(offset, 0, false);
	offset += 4; // maxBitrate
	view.setUint32(offset, 0, false);
	offset += 4; // avgBitrate

	// DecoderSpecificInfo (AudioSpecificConfig)
	esds[offset++] = 0x05; // tag
	esds[offset++] = decSpecificInfoSize; // size
	esds.set(audioSpecificConfig, offset);
	offset += decSpecificInfoSize;

	// SLConfigDescriptor
	esds[offset++] = 0x06; // tag
	esds[offset++] = 0x01; // size
	esds[offset++] = 0x02; // predefined = MP4

	return esds;
}

/**
 * Creates a dOps (Opus Specific) box.
 * See https://opus-codec.org/docs/opus_in_isobmff.html
 */
function createDOpsBox(channelCount: number, sampleRate: number, description?: string): Uint8Array {
	// If description is provided, it's the OpusHead without the magic signature
	if (description) {
		const opusHead = Hex.toBytes(description);
		const dOpsSize = 8 + opusHead.length;
		const dOps = new Uint8Array(dOpsSize);
		const view = new DataView(dOps.buffer);

		view.setUint32(0, dOpsSize, false);
		dOps[4] = 0x64; // 'd'
		dOps[5] = 0x4f; // 'O'
		dOps[6] = 0x70; // 'p'
		dOps[7] = 0x73; // 's'
		dOps.set(opusHead, 8);

		return dOps;
	}

	// Build minimal dOps box
	// dOps structure: Version (1) + OutputChannelCount (1) + PreSkip (2) +
	// InputSampleRate (4) + OutputGain (2) + ChannelMappingFamily (1)
	const dOpsSize = 8 + 11; // box header + content
	const dOps = new Uint8Array(dOpsSize);
	const view = new DataView(dOps.buffer);

	let offset = 0;
	view.setUint32(offset, dOpsSize, false);
	offset += 4;
	dOps[offset++] = 0x64; // 'd'
	dOps[offset++] = 0x4f; // 'O'
	dOps[offset++] = 0x70; // 'p'
	dOps[offset++] = 0x73; // 's'

	dOps[offset++] = 0; // Version
	dOps[offset++] = channelCount;
	view.setUint16(offset, 312, false);
	offset += 2; // PreSkip (typical value)
	view.setUint32(offset, sampleRate, false);
	offset += 4; // InputSampleRate
	view.setInt16(offset, 0, false);
	offset += 2; // OutputGain
	dOps[offset++] = 0; // ChannelMappingFamily (0 = mono/stereo)

	return dOps;
}

export interface DataSegmentOptions {
	/** Raw frame data */
	data: Uint8Array;
	/** Timestamp in timescale units */
	timestamp: number;
	/** Duration in timescale units */
	duration: number;
	/** Whether this is a keyframe */
	keyframe: boolean;
	/** Sequence number for this fragment */
	sequence: number;
	/** Track ID (default: 1) */
	trackId?: number;
}

/**
 * Encode a raw frame into a moof+mdat segment for MSE.
 *
 * @param opts - Options for the data segment
 * @returns The encoded moof+mdat segment
 */
export function encodeDataSegment(opts: DataSegmentOptions): Uint8Array {
	const { data, timestamp, duration, keyframe, sequence, trackId = 1 } = opts;

	// Sample flags:
	// - sample_depends_on: bits 25-24 (2 = does not depend on others for IDR, 1 = depends on others)
	// - sample_is_non_sync_sample: bit 16 (0 = sync/keyframe, 1 = non-sync)
	// For keyframe: depends_on=2 (0x02000000), non_sync=0
	// For non-keyframe: depends_on=1 (0x01000000), non_sync=1 (0x00010000)
	const sampleFlags = keyframe ? 0x02000000 : 0x01010000;

	// mfhd - Movie Fragment Header
	const mfhd: MovieFragmentHeaderBox = {
		type: "mfhd",
		version: 0,
		flags: 0,
		sequenceNumber: sequence,
	};

	// tfhd - Track Fragment Header
	// Flags: default-base-is-moof (0x020000)
	const tfhd: TrackFragmentHeaderBox = {
		type: "tfhd",
		version: 0,
		flags: 0x020000,
		trackId,
	};

	// tfdt - Track Fragment Base Media Decode Time
	const tfdt: TrackFragmentBaseMediaDecodeTimeBox = {
		type: "tfdt",
		version: 1, // version 1 for 64-bit baseMediaDecodeTime
		flags: 0,
		baseMediaDecodeTime: timestamp,
	};

	// trun - Track Run
	// Flags: data-offset-present (0x000001) | sample-duration-present (0x000100) |
	//        sample-size-present (0x000200) | sample-flags-present (0x000400)
	const trun: TrackRunBox = {
		type: "trun",
		version: 0,
		flags: 0x000001 | 0x000100 | 0x000200 | 0x000400,
		sampleCount: 1,
		dataOffset: 0, // Will be calculated after we know moof size
		samples: [
			{
				sampleDuration: duration,
				sampleSize: data.byteLength,
				sampleFlags,
			},
		],
	};

	// traf - Track Fragment
	const traf: TrackFragmentBox = {
		type: "traf",
		boxes: [tfhd, tfdt, trun],
	};

	// moof - Movie Fragment
	const moof: MovieFragmentBox = {
		type: "moof",
		boxes: [mfhd, traf],
	};

	// Write moof to calculate its size
	const moofBuffers = writeBoxes([moof]);
	let moofSize = 0;
	for (const buf of moofBuffers) {
		moofSize += buf.byteLength;
	}

	// Update trun.dataOffset to point to mdat data
	// dataOffset is relative to moof start, so it's moofSize + 8 (mdat header)
	trun.dataOffset = moofSize + 8;

	// Re-write moof with correct dataOffset
	const moofBuffersFinal = writeBoxes([moof]);
	moofSize = 0;
	for (const buf of moofBuffersFinal) {
		moofSize += buf.byteLength;
	}

	// mdat - Media Data
	// Need to ensure the data is a proper ArrayBuffer-backed Uint8Array for the library
	const mdatBuffer = new ArrayBuffer(data.byteLength);
	const mdatData = new Uint8Array(mdatBuffer);
	mdatData.set(data);
	const mdat: MediaDataBox = {
		type: "mdat",
		data: mdatData,
	};

	const mdatBuffers = writeBoxes([mdat]);
	let mdatSize = 0;
	for (const buf of mdatBuffers) {
		mdatSize += buf.byteLength;
	}

	// Concatenate all buffers
	const result = new Uint8Array(moofSize + mdatSize);
	let offset = 0;

	for (const buf of moofBuffersFinal) {
		result.set(new Uint8Array(buf.buffer, buf.byteOffset, buf.byteLength), offset);
		offset += buf.byteLength;
	}

	for (const buf of mdatBuffers) {
		result.set(new Uint8Array(buf.buffer, buf.byteOffset, buf.byteLength), offset);
		offset += buf.byteLength;
	}

	return result;
}
