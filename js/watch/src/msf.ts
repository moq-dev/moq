import type * as Catalog from "@moq/hang/catalog";
import { u53 } from "@moq/hang/catalog";
import * as Util from "@moq/hang/util";
import type * as Msf from "@moq/msf";
import { Path } from "@moq/net";
import { base64ToBytes } from "./base64";

const DEFAULT_SAMPLE_RATE = 48000;
const DEFAULT_NUMBER_OF_CHANNELS = 2;

function bytesToHex(bytes: Uint8Array): string {
	let hex = "";
	for (let i = 0; i < bytes.length; i++) hex += bytes[i].toString(16).padStart(2, "0");
	return hex;
}

interface ContainerInfo {
	container: Catalog.Container;
	description?: string;
}

// `packaging` declares how the bytes are framed, not an optional capability, so every value maps to
// its own container and an unrecognized one drops the rendition. Reading LOC's property block as
// legacy's VarInt prefix is exactly what a permissive fallback caused.
function toContainer(track: Msf.Track): ContainerInfo | undefined {
	let initBytes: Uint8Array | undefined;
	try {
		initBytes = track.initData ? base64ToBytes(track.initData) : undefined;
	} catch {
		initBytes = undefined;
	}

	const description = initBytes ? bytesToHex(initBytes) : undefined;

	switch (track.packaging) {
		case "cmaf":
			// Unusable without its init segment, so a missing or malformed one drops the rendition rather
			// than falling back to a different framing. hang's CMAF path reads the codec metadata from
			// that segment, so there is no separate description to surface.
			if (!track.initData || !initBytes) return undefined;
			return { container: { kind: "cmaf", init: track.initData }, description: undefined };
		case "loc":
			return { container: { kind: "loc" }, description };
		case "legacy":
			return { container: { kind: "legacy" }, description };
		default:
			return undefined;
	}
}

// Hang stores delay as whole milliseconds rounded up, and zero means the rendition is not behind.
// MSF writes a fractional millisecond, which u53 rejects.
function delayMillis(value: number): ReturnType<typeof u53> | undefined {
	if (value <= 0) return undefined;
	return u53(Math.ceil(value));
}

// Hang stores jitter as whole milliseconds; round MSF's fractional value up. A negative value goes
// to u53 unrounded, so it is rejected even between -1 and 0, where rounding up would give -0.
function jitterMillis(value: number): ReturnType<typeof u53> {
	return u53(value < 0 ? value : Math.ceil(value));
}

function toVideoConfig(track: Msf.Track): Catalog.VideoConfig | undefined {
	if (!track.codec) return undefined;

	const info = toContainer(track);
	if (!info) return undefined;
	const { container, description } = info;
	return {
		codec: track.codec,
		container,
		description,
		codedWidth: track.width != null ? u53(track.width) : undefined,
		codedHeight: track.height != null ? u53(track.height) : undefined,
		framerate: track.framerate,
		bitrate: track.bitrate != null ? u53(track.bitrate) : undefined,
		jitter: track.jitter != null ? jitterMillis(track.jitter) : undefined,
		delay: track.delay != null ? delayMillis(track.delay) : undefined,
	};
}

function toAudioConfig(track: Msf.Track): Catalog.AudioConfig | undefined {
	if (!track.codec) return undefined;

	const channels = (() => {
		if (!track.channelConfig) return DEFAULT_NUMBER_OF_CHANNELS;
		const parsed = Number.parseInt(track.channelConfig, 10);
		return Number.isFinite(parsed) ? parsed : DEFAULT_NUMBER_OF_CHANNELS;
	})();

	const info = toContainer(track);
	if (!info) return undefined;
	const { container, description } = info;
	return {
		codec: track.codec,
		container,
		description,
		sampleRate: u53(track.samplerate ?? DEFAULT_SAMPLE_RATE),
		numberOfChannels: u53(channels),
		bitrate: track.bitrate != null ? u53(track.bitrate) : undefined,
		// MSF cannot advertise decoder warmup, so derive Opus's seek pre-roll.
		warmup: track.codec === "opus" ? u53(Util.Opus.WARMUP) : undefined,
		jitter: track.jitter != null ? jitterMillis(track.jitter) : undefined,
		delay: track.delay != null ? delayMillis(track.delay) : undefined,
	};
}

// MSF namespaces are absolute, but each side sees paths under its own connection root. Draft
// catalogs repeat the publisher's full namespace on every track, so a namespace matching the
// catalog's path on whole trailing segments is the catalog seen from a different root.
function namesCatalog(namespace: Path.Valid, base: Path.Valid): boolean {
	if (namespace === base) return true;
	const a = Path.parts(namespace);
	const b = Path.parts(base);
	if (a.length === 0 || b.length === 0) return false;
	const endsWith = (long: string[], short: string[]) =>
		short.length <= long.length && short.every((part, i) => part === long[long.length - short.length + i]);
	return endsWith(a, b) || endsWith(b, a);
}

/** Convert an MSF catalog to a hang catalog Root. */
export function toHang(msf: Msf.Catalog, base?: Path.Valid): Catalog.Root {
	// Track names are untrusted keys; a null prototype keeps `__proto__` an ordinary rendition.
	const videoRenditions: Record<string, Catalog.VideoConfig> = Object.create(null);
	const audioRenditions: Record<string, Catalog.AudioConfig> = Object.create(null);
	const textRenditions: Record<string, Catalog.TextConfig> = Object.create(null);

	for (const track of msf.tracks) {
		// The track name is the rendition ID, so a later track with the name replaces the earlier
		// one, whatever namespace or role either has.
		delete videoRenditions[track.name];
		delete audioRenditions[track.name];
		delete textRenditions[track.name];
		let broadcast: Path.Relative | undefined;
		if (track.namespace !== undefined) {
			if (base === undefined)
				throw new Error(`MSF track ${track.name} namespace requires the catalog broadcast path`);
			let target: Path.Valid;
			try {
				target = Path.from(track.namespace);
			} catch {
				throw new Error(`MSF track ${track.name} namespace cannot be referenced from the catalog`);
			}
			if (!namesCatalog(target, base)) {
				const relative = Path.relative(target, base);
				if (relative === undefined)
					throw new Error(`MSF track ${track.name} namespace cannot be referenced from the catalog`);
				broadcast = relative;
			}
		}
		if (track.role === "video") {
			const config = toVideoConfig(track);
			if (config) videoRenditions[track.name] = { ...config, broadcast };
		} else if (track.role === "audio") {
			const config = toAudioConfig(track);
			if (config) audioRenditions[track.name] = { ...config, broadcast };
		} else if (
			track.role === "caption" ||
			track.role === "subtitle" ||
			(track.role &&
				track.role !== "audiodescription" &&
				track.role !== "signlanguage" &&
				["vtt", "ttml", "utf8"].includes(track.codec ?? ""))
		) {
			const info = toContainer(track);
			// A player selects usable renditions; an optional unsupported track cannot invalidate A/V.
			if (!info) continue;
			if (!track.codec) throw new Error(`MSF text track ${track.name} missing codec`);
			textRenditions[track.name] = {
				broadcast,
				format: track.codec,
				role: track.role,
				lang: track.lang,
				label: track.label,
				container: info.container,
				jitter: track.jitter != null ? delayMillis(track.jitter) : undefined,
				delay: track.delay != null ? delayMillis(track.delay) : undefined,
			};
		}
	}

	const root: Catalog.Root = {};

	if (Object.keys(videoRenditions).length > 0) {
		root.video = { renditions: videoRenditions };
	}

	if (Object.keys(audioRenditions).length > 0) {
		root.audio = { renditions: audioRenditions };
	}

	if (Object.keys(textRenditions).length > 0) root.text = { renditions: textRenditions };

	return root;
}
