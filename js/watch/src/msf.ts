import type * as Catalog from "@moq/hang/catalog";
import { u53 } from "@moq/hang/catalog";
import { Cmaf } from "@moq/hang/container";
import type * as Msf from "@moq/msf";

const DEFAULT_SAMPLE_RATE = 48000;
const DEFAULT_NUMBER_OF_CHANNELS = 2;

function base64ToBytes(b64: string): Uint8Array | undefined {
	try {
		const raw = atob(b64);
		const bytes = new Uint8Array(raw.length);
		for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
		return bytes;
	} catch {
		return undefined;
	}
}

function bytesToHex(bytes: Uint8Array): string {
	let hex = "";
	for (let i = 0; i < bytes.length; i++) hex += bytes[i].toString(16).padStart(2, "0");
	return hex;
}

interface ContainerInfo {
	container: Catalog.Container;
	description?: string;
}

function toContainer(track: Msf.Track): ContainerInfo {
	const initBytes = track.initData ? base64ToBytes(track.initData) : undefined;

	if (track.packaging === "cmaf" && initBytes) {
		try {
			const init = Cmaf.decodeInitSegment(initBytes);
			return {
				container: { kind: "cmaf", timescale: u53(init.timescale), trackId: u53(init.trackId) },
				description: init.description ? bytesToHex(init.description) : undefined,
			};
		} catch (err) {
			console.warn("failed to parse MSF cmaf initData, falling back to legacy", err);
		}
	}

	return {
		container: { kind: "legacy" },
		description: initBytes ? bytesToHex(initBytes) : undefined,
	};
}

function toVideoConfig(track: Msf.Track): Catalog.VideoConfig | undefined {
	if (!track.codec) return undefined;

	const { container, description } = toContainer(track);
	return {
		codec: track.codec,
		container,
		description,
		codedWidth: track.width != null ? u53(track.width) : undefined,
		codedHeight: track.height != null ? u53(track.height) : undefined,
		framerate: track.framerate,
		bitrate: track.bitrate != null ? u53(track.bitrate) : undefined,
		jitter: track.jitter != null ? u53(track.jitter) : undefined,
	};
}

function toAudioConfig(track: Msf.Track): Catalog.AudioConfig | undefined {
	if (!track.codec) return undefined;

	const channels = (() => {
		if (!track.channelConfig) return DEFAULT_NUMBER_OF_CHANNELS;
		const parsed = Number.parseInt(track.channelConfig, 10);
		return Number.isFinite(parsed) ? parsed : DEFAULT_NUMBER_OF_CHANNELS;
	})();

	const { container, description } = toContainer(track);
	return {
		codec: track.codec,
		container,
		description,
		sampleRate: u53(track.samplerate ?? DEFAULT_SAMPLE_RATE),
		numberOfChannels: u53(channels),
		bitrate: track.bitrate != null ? u53(track.bitrate) : undefined,
		jitter: track.jitter != null ? u53(track.jitter) : undefined,
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
