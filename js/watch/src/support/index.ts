/**
 * Feature detection for playback: which codecs decode, and whether WebTransport works.
 *
 * @module
 */

import { Connection } from "@moq/net";

// https://bugzilla.mozilla.org/show_bug.cgi?id=1967793
const isFirefox = navigator.userAgent.toLowerCase().includes("firefox");

/** How well a feature works: natively, through a fallback, or not at all. */
export type Level = "full" | "partial" | "none";

/** Whether a codec decodes in hardware and in software. */
export type Codec = {
	hardware?: boolean; // undefined when we can't detect hardware acceleration
	software: boolean;
};

/** Which audio codecs decode. */
export type Audio = {
	aac: Level;
	opus: Level;
};

/** Which video codecs decode. */
export type Video = {
	h264: Codec;
	h265: Codec;
	vp8: Codec;
	vp9: Codec;
	av1: Codec;
};

/** The full playback support report. */
export type Full = {
	webtransport: Level;
	audio: {
		decoding: Audio;
		render: boolean;
	};
	video: {
		decoding: Video | undefined;
		render: boolean;
	};
};

// Pick a codec string for each codec.
// This is not strictly correct, as browsers may not support every profile or level.
const CODECS = {
	aac: "mp4a.40.2",
	opus: "opus",
	av1: "av01.0.08M.08",
	h264: "avc1.640028",
	h265: "hev1.1.6.L93.B0",
	vp9: "vp09.00.10.08",
	vp8: "vp8",
};

async function audioDecoderSupported(codec: keyof typeof CODECS): Promise<boolean> {
	if (!globalThis.AudioDecoder) return false;

	const res = await AudioDecoder.isConfigSupported({
		codec: CODECS[codec],
		numberOfChannels: 2,
		sampleRate: 48000,
	});

	return res.supported === true;
}

async function videoDecoderSupported(codec: keyof typeof CODECS): Promise<Codec> {
	const software = await VideoDecoder.isConfigSupported({
		codec: CODECS[codec],
		hardwareAcceleration: "prefer-software",
	});

	const hardware = await VideoDecoder.isConfigSupported({
		codec: CODECS[codec],
		hardwareAcceleration: "prefer-hardware",
	});

	// We can't reliably detect hardware encoding on Firefox: https://github.com/w3c/webcodecs/issues/896
	const unknown = isFirefox || hardware.config?.hardwareAcceleration !== "prefer-hardware";

	return {
		hardware: unknown ? undefined : hardware.supported === true,
		software: software.supported === true,
	};
}

/** Detect what this browser supports for playback. */
export async function isSupported(): Promise<Full> {
	return {
		// Report "partial" when @moq/net forces the WebSocket fallback.
		webtransport: Connection.isWebTransportSupported() ? "full" : "partial",
		audio: {
			decoding: {
				// The libav fallback decodes AAC only where it installs, which is wherever AudioDecoder is missing.
				aac: (await audioDecoderSupported("aac")) ? "full" : globalThis.AudioDecoder ? "none" : "partial",
				opus: (await audioDecoderSupported("opus")) ? "full" : "partial",
			},
			render: typeof AudioContext !== "undefined" && typeof AudioBufferSourceNode !== "undefined",
		},
		video: {
			decoding:
				typeof VideoDecoder !== "undefined"
					? {
							h264: await videoDecoderSupported("h264"),
							h265: await videoDecoderSupported("h265"),
							vp8: await videoDecoderSupported("vp8"),
							vp9: await videoDecoderSupported("vp9"),
							av1: await videoDecoderSupported("av1"),
						}
					: undefined,
			render: typeof OffscreenCanvas !== "undefined" && typeof CanvasRenderingContext2D !== "undefined",
		},
	};
}
