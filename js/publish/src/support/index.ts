// https://bugzilla.mozilla.org/show_bug.cgi?id=1967793
const isFirefox = navigator.userAgent.toLowerCase().includes("firefox");

export type Partial = "full" | "partial" | "none";

export type Codec = {
	hardware?: boolean; // undefined when we can't detect hardware acceleration
	software: boolean;
};

export type Audio = {
	aac: boolean;
	opus: Partial;
};

export type Video = {
	h264: Codec;
	h265: Codec;
	vp8: Codec;
	vp9: Codec;
	av1: Codec;
};

export type Full = {
	webtransport: Partial;
	audio: {
		capture: boolean;
		encoding: Audio;
	};
	video: {
		capture: Partial;
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

async function videoEncoderSupported(codec: keyof typeof CODECS): Promise<Codec> {
	const software = await VideoEncoder.isConfigSupported({
		codec: CODECS[codec],
		width: 1280,
		height: 720,
		hardwareAcceleration: "prefer-software",
	});

	// We can't reliably detect hardware encoding on Firefox: https://github.com/w3c/webcodecs/issues/896
	const hardware = await VideoEncoder.isConfigSupported({
		codec: CODECS[codec],
		width: 1280,
		height: 720,
		hardwareAcceleration: "prefer-hardware",
	});

	const unknown = isFirefox || hardware.config?.hardwareAcceleration !== "prefer-hardware";

	return {
		hardware: unknown ? undefined : hardware.supported === true,
		software: software.supported === true,
	};
}

export async function isSupported(): Promise<Full> {
	return {
		webtransport: typeof WebTransport !== "undefined" ? "full" : "partial",
		audio: {
			capture: typeof AudioWorkletNode !== "undefined",
			encoding: {
				aac: await audioEncoderSupported("aac"),
				opus: (await audioEncoderSupported("opus")) ? "full" : "partial",
			},
		},
		video: {
			capture:
				// We have a fallback for MediaStreamTrackProcessor, but it's pretty gross so no full points.
				// @ts-expect-error No typescript types yet.
				typeof MediaStreamTrackProcessor !== "undefined"
					? "full"
					: typeof OffscreenCanvas !== "undefined"
						? "partial"
						: "none",
			encoding:
				typeof VideoEncoder !== "undefined"
					? {
							h264: await videoEncoderSupported("h264"),
							h265: await videoEncoderSupported("h265"),
							vp8: await videoEncoderSupported("vp8"),
							vp9: await videoEncoderSupported("vp9"),
							av1: await videoEncoderSupported("av1"),
						}
					: undefined,
		},
	};
}
