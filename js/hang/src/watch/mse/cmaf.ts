// NOTE: Vibe coded and untested

import {
	type AudioSampleEntryBox,
	type ChunkOffsetBox,
	type DataEntryUrlBox,
	type DataInformationBox,
	type DataReferenceBox,
	type DecodingTimeToSampleBox,
	type FileTypeBox,
	type HandlerReferenceBox,
	type MediaBox,
	type MediaHeaderBox,
	type MediaInformationBox,
	type MovieBox,
	type MovieExtendsBox,
	type MovieHeaderBox,
	type SampleDescriptionBox,
	type SampleSizeBox,
	type SampleTableBox,
	type SampleToChunkBox,
	type SoundMediaHeaderBox,
	type TrackBox,
	type TrackExtendsBox,
	type TrackHeaderBox,
	type VideoMediaHeaderBox,
	type VisualSampleEntryBox,
	writeIsoBoxes,
} from "@svta/cml-iso-bmff";

import type * as Catalog from "../../catalog";
import * as Hex from "../../util/hex";

// Identity matrix for tkhd/mvhd (stored as 16.16 fixed point)
const IDENTITY_MATRIX = [0x00010000, 0, 0, 0, 0x00010000, 0, 0, 0, 0x40000000];

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

	// TODO CMAF doesn't require this... so it'll break at some point.
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
		width: codedWidth << 16, // 16.16 fixed point
		height: codedHeight << 16,
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

	// Build avcC box - the description from WebCodecs is the avcC payload (without box header)
	// We need to create the complete box including the 8-byte header
	const avcCPayload = Hex.toBytes(description);
	const avcCSize = 8 + avcCPayload.length; // 4 bytes size + 4 bytes type + payload
	const avcCBuffer = new Uint8Array(avcCSize);
	const avcCView = new DataView(avcCBuffer.buffer);
	avcCView.setUint32(0, avcCSize, false); // size (big-endian)
	avcCBuffer[4] = 0x61; // 'a'
	avcCBuffer[5] = 0x76; // 'v'
	avcCBuffer[6] = 0x63; // 'c'
	avcCBuffer[7] = 0x43; // 'C'
	avcCBuffer.set(avcCPayload, 8);

	// The library accepts ArrayBufferView directly for raw boxes
	const avcCBox = avcCBuffer;

	// avc1 - Visual Sample Entry
	const avc1: VisualSampleEntryBox<"avc1"> = {
		type: "avc1",
		reserved1: [0, 0, 0, 0, 0, 0],
		dataReferenceIndex: 1,
		preDefined1: 0,
		reserved2: 0,
		preDefined2: [0, 0, 0],
		width: codedWidth,
		height: codedHeight,
		horizresolution: 0x00480000, // 72 dpi
		vertresolution: 0x00480000,
		reserved3: 0,
		frameCount: 1,
		compressorName: new Array(32).fill(0), // Empty compressor name
		depth: 0x0018, // 24-bit color
		preDefined3: -1, // 0xFFFF
		boxes: [avcCBox],
	};

	// stsd - Sample Description Box
	const stsd: SampleDescriptionBox = {
		type: "stsd",
		version: 0,
		flags: 0,
		entryCount: 1,
		entries: [avc1],
	};

	// stts - Decoding Time to Sample (empty for fragmented)
	const stts: DecodingTimeToSampleBox = {
		type: "stts",
		version: 0,
		flags: 0,
		entryCount: 0,
		entries: [],
	};

	// stsc - Sample to Chunk (empty for fragmented)
	const stsc: SampleToChunkBox = {
		type: "stsc",
		version: 0,
		flags: 0,
		entryCount: 0,
		entries: [],
	};

	// stsz - Sample Size (empty for fragmented)
	const stsz: SampleSizeBox = {
		type: "stsz",
		version: 0,
		flags: 0,
		sampleSize: 0,
		sampleCount: 0,
	};

	// stco - Chunk Offset (empty for fragmented)
	const stco: ChunkOffsetBox = {
		type: "stco",
		version: 0,
		flags: 0,
		entryCount: 0,
		chunkOffset: [],
	};

	// stbl - Sample Table Box
	const stbl: SampleTableBox = {
		type: "stbl",
		boxes: [stsd, stts, stsc, stsz, stco],
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
	const buffers = writeIsoBoxes([ftyp, moov]);
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

	// Build codec-specific sample entry
	const sampleEntry = createAudioSampleEntry(codec, sampleRate, numberOfChannels, description);

	// stsd - Sample Description Box
	const stsd: SampleDescriptionBox = {
		type: "stsd",
		version: 0,
		flags: 0,
		entryCount: 1,
		entries: [sampleEntry],
	};

	// stts - Decoding Time to Sample (empty for fragmented)
	const stts: DecodingTimeToSampleBox = {
		type: "stts",
		version: 0,
		flags: 0,
		entryCount: 0,
		entries: [],
	};

	// stsc - Sample to Chunk (empty for fragmented)
	const stsc: SampleToChunkBox = {
		type: "stsc",
		version: 0,
		flags: 0,
		entryCount: 0,
		entries: [],
	};

	// stsz - Sample Size (empty for fragmented)
	const stsz: SampleSizeBox = {
		type: "stsz",
		version: 0,
		flags: 0,
		sampleSize: 0,
		sampleCount: 0,
	};

	// stco - Chunk Offset (empty for fragmented)
	const stco: ChunkOffsetBox = {
		type: "stco",
		version: 0,
		flags: 0,
		entryCount: 0,
		chunkOffset: [],
	};

	// stbl - Sample Table Box
	const stbl: SampleTableBox = {
		type: "stbl",
		boxes: [stsd, stts, stsc, stsz, stco],
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
	const buffers = writeIsoBoxes([ftyp, moov]);
	const totalLength = buffers.reduce((sum, buf) => sum + buf.byteLength, 0);
	const result = new Uint8Array(totalLength);

	let offset = 0;
	for (const buf of buffers) {
		result.set(new Uint8Array(buf.buffer, buf.byteOffset, buf.byteLength), offset);
		offset += buf.byteLength;
	}

	return result;
}

function createAudioSampleEntry(
	codec: string,
	sampleRate: number,
	channelCount: number,
	description?: string,
): AudioSampleEntryBox {
	if (codec.startsWith("mp4a")) {
		return createMp4aSampleEntry(sampleRate, channelCount, description);
	} else if (codec === "opus") {
		// Cast needed because "Opus" is not in the library's AudioSampleEntryType union
		return createOpusSampleEntry(sampleRate, channelCount, description) as unknown as AudioSampleEntryBox;
	}
	throw new Error(`Unsupported audio codec: ${codec}`);
}

function createMp4aSampleEntry(
	sampleRate: number,
	channelCount: number,
	description?: string,
): AudioSampleEntryBox<"mp4a"> {
	// Build esds box with AudioSpecificConfig
	const esdsBox = createEsdsBox(description);

	return {
		type: "mp4a",
		reserved1: [0, 0, 0, 0, 0, 0],
		dataReferenceIndex: 1,
		reserved2: [0, 0],
		channelcount: channelCount,
		samplesize: 16,
		preDefined: 0,
		reserved3: 0,
		samplerate: sampleRate << 16, // 16.16 fixed point
		boxes: [esdsBox],
	};
}

function createOpusSampleEntry(sampleRate: number, channelCount: number, description?: string) {
	// Build dOps box
	const dOpsBox = createDOpsBox(channelCount, sampleRate, description);

	return {
		type: "Opus",
		reserved1: [0, 0, 0, 0, 0, 0],
		dataReferenceIndex: 1,
		reserved2: [0, 0],
		channelcount: channelCount,
		samplesize: 16,
		preDefined: 0,
		reserved3: 0,
		samplerate: sampleRate << 16,
		boxes: [dOpsBox],
	};
}

/**
 * Creates an esds (Elementary Stream Descriptor) box for AAC.
 * The description from WebCodecs is the AudioSpecificConfig.
 */
function createEsdsBox(description?: string): Uint8Array {
	const audioSpecificConfig = description ? Hex.toBytes(description) : new Uint8Array(0);

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
