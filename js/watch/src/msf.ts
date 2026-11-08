import type * as Catalog from "@moq/hang/catalog";
import { DrmSystemSchema, u53 } from "@moq/hang/catalog";
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
// MSF writes a fractional millisecond, which u53 rejects. A negative value goes to u53 unrounded, so
// it is rejected like a negative jitter rather than read as absent.
function delayMillis(value: number): ReturnType<typeof u53> | undefined {
	if (value === 0) return undefined;
	return u53(value < 0 ? value : Math.ceil(value));
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

const HYPHENATED = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const SIMPLE = /^[0-9a-f]{32}$/i;

/**
 * A UUID in lowercase hyphenated form, from any form moq-mux's `uuid` parse accepts: simple,
 * hyphenated, braced, or URN. `undefined` for anything else.
 */
function normalizeUuid(text: string): string | undefined {
	let hyphenated: string;
	if (text.length === 32) {
		if (!SIMPLE.test(text)) return undefined;
		const at = [0, 8, 12, 16, 20, 32];
		hyphenated = at
			.slice(1)
			.map((end, i) => text.slice(at[i], end))
			.join("-");
	} else if (text.length === 36) {
		hyphenated = text;
	} else if (text.length === 38 && text.startsWith("{") && text.endsWith("}")) {
		hyphenated = text.slice(1, -1);
	} else if (text.length === 45 && text.slice(0, 9).toLowerCase() === "urn:uuid:") {
		hyphenated = text.slice(9);
	} else {
		return undefined;
	}
	return HYPHENATED.test(hyphenated) ? hyphenated.toLowerCase() : undefined;
}

/**
 * The hang `drm` entry one MSF content protection describes, or `undefined` when the hang catalog
 * cannot read it.
 *
 * Mirrors moq-mux: a hang entry names one key, so an entry naming several default keys is not read.
 */
function toEntry(cp: Msf.ContentProtection): Catalog.DrmSystem | undefined {
	if (cp.defaultKID.length !== 1) return undefined;
	const system = normalizeUuid(cp.drmSystem.systemID);
	const kid = normalizeUuid(cp.defaultKID[0]);
	if (system === undefined || kid === undefined) return undefined;
	// The scheme, the servers' URLs, and the pssh are checked the way the hang catalog reads them.
	const entry = DrmSystemSchema.safeParse({
		system,
		scheme: cp.scheme,
		kid,
		pssh: cp.drmSystem.pssh,
		license: cp.drmSystem.laURL,
		certificate: cp.drmSystem.certURL,
	});
	return entry.success ? entry.data : undefined;
}

/**
 * The hang `drm` entries an MSF catalog's `contentProtections` describe, keyed by `refID`.
 *
 * An entry the hang catalog cannot read is kept as-is, so the tracks it protects stay protected.
 */
function toDrm(msf: Msf.Catalog): Record<string, Catalog.DrmEntry> {
	const drm: Record<string, Catalog.DrmEntry> = Object.create(null);
	for (const cp of msf.contentProtections ?? []) drm[cp.refID] = toEntry(cp) ?? { ...cp };
	return drm;
}

/** Convert an MSF catalog to a hang catalog Root. */
export function toHang(msf: Msf.Catalog, base?: Path.Valid): Catalog.Root {
	const drm = toDrm(msf);
	// A track's refIDs are its entries' IDs; @moq/msf refuses a refID the root lacks.
	const protection = (track: Msf.Track): { drm?: string[] } =>
		track.contentProtectionRefIDs?.length ? { drm: track.contentProtectionRefIDs } : {};
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
			if (config) videoRenditions[track.name] = { ...config, broadcast, ...protection(track) };
		} else if (track.role === "audio") {
			const config = toAudioConfig(track);
			if (config) audioRenditions[track.name] = { ...config, broadcast, ...protection(track) };
		} else if (
			track.role === "caption" ||
			track.role === "subtitle" ||
			(track.role &&
				track.role !== "audiodescription" &&
				track.role !== "signlanguage" &&
				["vtt", "ttml", "utf8"].includes(track.codec ?? ""))
		) {
			// A hang text rendition has no `drm` to say it is protected, so it would read as clear.
			if (track.contentProtectionRefIDs?.length) continue;
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

	if (Object.keys(drm).length > 0) root.drm = drm;

	return root;
}
