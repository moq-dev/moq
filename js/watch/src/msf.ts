import type * as Catalog from "@moq/hang/catalog";
import { u53 } from "@moq/hang/catalog";
import type * as Msf from "@moq/msf";

const DEFAULT_SAMPLE_RATE = 48000;
const DEFAULT_NUMBER_OF_CHANNELS = 2;

// Convert base64 string to hex string, returning undefined on invalid input.
function base64ToHex(b64: string): string | undefined {
	try {
		const raw = atob(b64);
		let hex = "";
		for (let i = 0; i < raw.length; i++) {
			hex += raw.charCodeAt(i).toString(16).padStart(2, "0");
		}
		return hex;
	} catch {
		return undefined;
	}
}

function toContainer(track: Msf.Track): Catalog.Container {
	if (track.packaging === "cmaf" && track.initData) {
		return { kind: "cmaf", initData: track.initData };
	}
	return { kind: "legacy" };
}

function toVideoConfig(track: Msf.Track): Catalog.VideoConfig | undefined {
	if (!track.codec) return undefined;

	return {
		codec: track.codec,
		container: toContainer(track),
		description: track.packaging !== "cmaf" && track.initData ? base64ToHex(track.initData) : undefined,
		codedWidth: track.width != null ? u53(track.width) : undefined,
		codedHeight: track.height != null ? u53(track.height) : undefined,
		framerate: track.framerate,
		bitrate: track.bitrate != null ? u53(track.bitrate) : undefined,
	};
}

function toAudioConfig(track: Msf.Track): Catalog.AudioConfig | undefined {
	if (!track.codec) return undefined;

	return {
		codec: track.codec,
		container: toContainer(track),
		description: track.packaging !== "cmaf" && track.initData ? base64ToHex(track.initData) : undefined,
		sampleRate: u53(track.samplerate ?? DEFAULT_SAMPLE_RATE),
		numberOfChannels: u53(
			(() => {
				if (!track.channelConfig) return DEFAULT_NUMBER_OF_CHANNELS;
				const parsed = Number.parseInt(track.channelConfig, 10);
				return Number.isFinite(parsed) ? parsed : DEFAULT_NUMBER_OF_CHANNELS;
			})(),
		),
		bitrate: track.bitrate != null ? u53(track.bitrate) : undefined,
	};
}

/** Convert an MSF catalog to a hang catalog Root. */
export function toHang(msf: Msf.Catalog): Catalog.Root {
	const videoRenditions: Record<string, Catalog.VideoConfig> = {};
	const audioRenditions: Record<string, Catalog.AudioConfig> = {};

	for (const track of msf.tracks) {
		if (track.role === "video") {
			const config = toVideoConfig(track);
			if (config) videoRenditions[track.name] = config;
		} else if (track.role === "audio") {
			const config = toAudioConfig(track);
			if (config) audioRenditions[track.name] = config;
		}
	}

	const root: Catalog.Root = {};

	if (Object.keys(videoRenditions).length > 0) {
		root.video = { renditions: videoRenditions };
	}

	if (Object.keys(audioRenditions).length > 0) {
		root.audio = { renditions: audioRenditions };
	}

	return root;
}
