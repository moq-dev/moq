/**
 * Feature detection for publishing: which codecs encode, whether capture works, and
 * whether WebTransport works.
 *
 * @module
 */
import { Connection } from "@moq/net";
import { workerSupported } from "../video/processor";
import { probe } from "./video";

export type Level = "full" | "partial" | "none";

export type Codec = {
	hardware?: boolean; // undefined when we can't detect hardware acceleration
	software: boolean;
};

export type Audio = {
	aac: boolean;
	opus: Level;
};

export type Video = {
	h264: Codec;
	h265: Codec;
	vp8: Codec;
	vp9: Codec;
	av1: Codec;
};

export type Full = {
	webtransport: Level;
	audio: {
		capture: boolean;
		encoding: Audio;
	};
	video: {
		capture: Level;
		encoding: Video | undefined;
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

async function audioEncoderSupported(codec: keyof typeof CODECS): Promise<boolean> {
	if (!globalThis.AudioEncoder) return false;

	const res = await AudioEncoder.isConfigSupported({
		codec: CODECS[codec],
		numberOfChannels: 2,
		sampleRate: 48000,
	});

	return res.supported === true;
}

export async function isSupported(): Promise<Full> {
	return {
		// Report "partial" when @moq/net forces the WebSocket fallback.
		webtransport: Connection.isWebTransportSupported() ? "full" : "partial",
		audio: {
			capture: typeof AudioWorkletNode !== "undefined",
			encoding: {
				aac: await audioEncoderSupported("aac"),
				opus: (await audioEncoderSupported("opus")) ? "full" : "partial",
			},
		},
		video: {
			capture:
				// Chrome has MediaStreamTrackProcessor on the window; Safari and Firefox only in a
				// worker, which we hop through. Either way it's the native pipeline, so full points.
				typeof MediaStreamTrackProcessor !== "undefined" || (await workerSupported())
					? "full"
					: // The fallback drives a <video> element via requestVideoFrameCallback, which is
						// gross and stops producing frames when the window isn't composited.
						"requestVideoFrameCallback" in HTMLVideoElement.prototype
						? "partial"
						: "none",
			encoding:
				typeof VideoEncoder !== "undefined"
					? {
							h264: await probe(CODECS.h264),
							h265: await probe(CODECS.h265),
							vp8: await probe(CODECS.vp8),
							vp9: await probe(CODECS.vp9),
							av1: await probe(CODECS.av1),
						}
					: undefined,
		},
	};
}
