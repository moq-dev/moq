import * as Util from "@moq/hang/util";

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
	const base = { codec: CODECS[codec], width: 1280, height: 720 };

	const software = await VideoEncoder.isConfigSupported({ ...base, hardwareAcceleration: "prefer-software" });

	// On Apple Silicon Safari, VideoToolbox hardware-encodes only H.264 and HEVC; Safari reports the
	// others as prefer-hardware supported even though they run on software libvpx, so key off the
	// codec rather than the echoed (and unreliable) hint, and skip the probe it would ignore. The
	// same H.264/HEVC-only set drives hardwareCodecOrder in ../video/codecs.ts.
	if (Util.Hacks.isSafari) {
		const hardwareCapable = codec === "h264" || codec === "h265";
		const hardware = hardwareCapable
			? await VideoEncoder.isConfigSupported({ ...base, hardwareAcceleration: "prefer-hardware" })
			: undefined;
		return {
			hardware: hardware?.supported === true,
			software: software.supported === true,
		};
	}

	// We can't reliably detect hardware encoding on Firefox: https://github.com/w3c/webcodecs/issues/896
	const hardware = await VideoEncoder.isConfigSupported({ ...base, hardwareAcceleration: "prefer-hardware" });

	const unknown = Util.Hacks.isFirefox || hardware.config?.hardwareAcceleration !== "prefer-hardware";

	return {
		hardware: unknown ? undefined : hardware.supported === true,
		software: software.supported === true,
	};
}

export async function isSupported(): Promise<Full> {
	// @ts-expect-error MediaStreamTrackProcessor has no TypeScript types yet. Keep the suppression on
	// this line only so it doesn't mask type errors in the capture expression below.
	const mainThreadCapture = typeof MediaStreamTrackProcessor !== "undefined";

	return {
		// Firefox drops server-initiated bidi streams, and reading datagrams kills the session on
		// Safari, so both are forced onto the WebSocket fallback even though they define
		// `WebTransport`. Report "partial" to surface the degraded path in UI.
		webtransport:
			typeof WebTransport !== "undefined" && !Util.Hacks.isFirefox && !Util.Hacks.isSafari ? "full" : "partial",
		audio: {
			capture: typeof AudioWorkletNode !== "undefined",
			encoding: {
				aac: await audioEncoderSupported("aac"),
				opus: (await audioEncoderSupported("opus")) ? "full" : "partial",
			},
		},
		video: {
			// Chrome exposes MediaStreamTrackProcessor on the main thread; WebKit 18+ (incl. iOS
			// Chrome/Firefox) uses it in a Worker (see video/polyfill.ts). Older/undetectable WebKit falls
			// back to the rAF polyfill, so it's only "partial".
			capture:
				mainThreadCapture || Util.Hacks.safariWorkerCapture
					? "full"
					: Util.Hacks.isSafari || typeof OffscreenCanvas !== "undefined"
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
