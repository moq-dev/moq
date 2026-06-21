/**
 * MoQ publish demo built on the <moq-publish-ui> web component.
 *
 * The component owns capture (camera / screen / file / mic), preview, go-live,
 * and mute. This demo adds on top of it:
 *
 *   1. A side panel of *encoder* settings. Each defaults to "auto" (the field is
 *      omitted so the encoder picks); we drive the broadcast's encoder signals
 *      directly and show the negotiated value beside each control once live.
 *   2. A toggle between a raw-capture preview and an "encoded" preview that
 *      decodes a copy of the stream (what viewers actually receive).
 *   3. A custom `meta.json` track carried *within* the broadcast.
 *   4. Live graphs (capture rate, upload-bandwidth estimate, round trip). The
 *      publish API exposes no encoded-byte counter, so these are the honestly-
 *      observable signals.
 */

import "./highlight";
import "@moq/publish/element"; // defines <moq-publish>
import "@moq/publish/ui"; // defines <moq-publish-ui>
import { type Audio, Json, type Net, Signals } from "@moq/publish";
import type MoqPublish from "@moq/publish/element";
import MoqPublishSupport from "@moq/publish/support/element";
import { formatBitrate, formatFps, graph } from "./viz";

/** Re-exported so bundlers keep the `<moq-publish-support>` element registration. */
export { MoqPublishSupport };

// Injected by Vite (see justfile). Defaults to the local relay.
const RELAY_URL = import.meta.env.VITE_RELAY_URL ?? "http://localhost:4443";

const $ = <T extends HTMLElement>(id: string): T => {
	const el = document.getElementById(id);
	if (!el) throw new Error(`missing #${id}`);
	return el as T;
};

// The component builds its Broadcast in the constructor, so `.broadcast` is ready
// as soon as the element upgrades. `broadcast.video.hd` and `broadcast.audio` are
// the encoders whose signals we drive below.
const publish = $<MoqPublish>("publish");
publish.url = RELAY_URL;

// ---------------------------------------------------------------------------
// Connection + broadcast name (editable)
// ---------------------------------------------------------------------------

const relayEl = $<HTMLInputElement>("relay-url");
relayEl.value = RELAY_URL;
relayEl.addEventListener("change", () => {
	try {
		publish.url = new URL(relayEl.value.trim());
	} catch {
		// Revert invalid input to the last good URL.
		relayEl.value = publish.url?.toString() ?? RELAY_URL;
	}
});

const nameEl = $<HTMLInputElement>("broadcast-name");
nameEl.value = String(publish.name);
nameEl.addEventListener("change", () => {
	const v = nameEl.value.trim();
	if (v) publish.name = v;
});

// Toggle the preview between the raw capture ("source") and a decoded copy of the
// encoded stream ("encoded"). Defaults to off (raw) to avoid the extra encode +
// decode unless the user wants to inspect codec artifacts.
const encodedEl = $<HTMLInputElement>("encoded-preview");
const syncPreview = () => publish.setAttribute("preview", encodedEl.checked ? "encoded" : "source");
encodedEl.addEventListener("change", syncPreview);
syncPreview();

// ---------------------------------------------------------------------------
// Encoder settings - reactive Signals the broadcast's encoders subscribe to.
// ---------------------------------------------------------------------------
//
// Video knobs default to undefined / "" meaning "auto": we omit the field so the
// encoder picks. The negotiated result shows up in the *-actual spans below.

const codec = new Signals.Signal<string | undefined>(undefined);
const resolution = new Signals.Signal(""); // "" => auto
const framerate = new Signals.Signal<number | undefined>(undefined);
const bitrateKbps = new Signals.Signal<number | undefined>(undefined);
const keyframeMs = new Signals.Signal<number | undefined>(undefined);

// Audio encode. Like the video knobs, undefined / "" means "auto" (omit the
// field so the encoder picks). Only Opus exists today.
const audioCodecKind = new Signals.Signal("opus");
const volume = new Signals.Signal(1);
const sampleRate = new Signals.Signal<number | undefined>(undefined);
const channelCount = new Signals.Signal<number | undefined>(undefined);

// Opus-specific knobs (the "Opus options" panel), mapping 1:1 onto OpusConfig.
const opusBitrateKbps = new Signals.Signal<number | undefined>(undefined);
const opusFrameDuration = new Signals.Signal<number | undefined>(undefined); // ms (2.5 to 60)
const opusComplexity = new Signals.Signal<number | undefined>(undefined); // 0 (fast) … 10 (best)
const opusFec = new Signals.Signal(false); // in-band forward error correction
const opusPacketLoss = new Signals.Signal<number | undefined>(undefined); // expected loss %
const opusDtx = new Signals.Signal(false); // discontinuous transmission (silence)

const ui = new Signals.Effect();

// Compose the WebCodecs/MoQ video encoder config and push it onto the HD
// rendition. Undefined fields are omitted, so the encoder auto-sizes them.
ui.run((effect) => {
	const res = effect.get(resolution);
	const [w, h] = res ? res.split("x").map(Number) : [undefined, undefined];
	const br = effect.get(bitrateKbps);
	const kf = effect.get(keyframeMs);
	publish.broadcast.video.hd.config.set({
		codec: effect.get(codec),
		maxPixels: w && h ? w * h : undefined,
		maxBitrate: br != null ? br * 1000 : undefined,
		keyframeInterval: kf != null ? (kf as Net.Time.Milli) : undefined,
		frameRate: effect.get(framerate),
	});
});

// Audio general settings (volume gain, output sample rate, channel mix).
ui.run((effect) => {
	publish.broadcast.audio.volume.set(effect.get(volume));
	publish.broadcast.audio.sampleRate.set(effect.get(sampleRate));
	publish.broadcast.audio.channelCount.set(effect.get(channelCount));
});

// Compose the structured audio codec config; today only Opus. Undefined knobs
// are omitted so the encoder auto-sizes them.
ui.run((effect) => {
	if (effect.get(audioCodecKind) !== "opus") return;
	const bitrate = effect.get(opusBitrateKbps);
	const frameDuration = effect.get(opusFrameDuration);
	const complexity = effect.get(opusComplexity);
	const packetLoss = effect.get(opusPacketLoss);
	const config: Audio.OpusConfig = {
		mime: "opus",
		...(bitrate != null ? { bitrate: bitrate * 1000 } : {}),
		...(frameDuration != null ? { frameDuration } : {}),
		...(complexity != null ? { complexity } : {}),
		...(packetLoss != null ? { packetlossperc: packetLoss } : {}),
		useinbandfec: effect.get(opusFec),
		usedtx: effect.get(opusDtx),
	};
	publish.broadcast.audio.codec.set(config);
});

// ---------------------------------------------------------------------------
// Input bindings (DOM -> Signal)
// ---------------------------------------------------------------------------

// A required number input: ignore empty / non-numeric so typing never pushes a
// transient 0 or NaN onto the encoder.
const bindNumber = (id: string, signal: Signals.Signal<number>) => {
	const el = $<HTMLInputElement | HTMLSelectElement>(id);
	const sync = () => {
		const n = Number(el.value);
		if (el.value.trim() !== "" && Number.isFinite(n)) signal.set(n);
	};
	sync();
	el.addEventListener("input", sync);
};

// An optional number input where empty means "auto" (undefined).
const bindOptionalNumber = (id: string, signal: Signals.Signal<number | undefined>) => {
	const el = $<HTMLInputElement>(id);
	const sync = () => {
		const v = el.value.trim();
		const n = Number(v);
		signal.set(v !== "" && Number.isFinite(n) ? n : undefined);
	};
	sync();
	el.addEventListener("input", sync);
};

// An optional select where the empty value ("Auto") means undefined.
const bindOptionalSelect = (id: string, signal: Signals.Signal<number | undefined>) => {
	const el = $<HTMLSelectElement>(id);
	const sync = () => signal.set(el.value ? Number(el.value) : undefined);
	sync();
	el.addEventListener("change", sync);
};

const bindCheckbox = (id: string, signal: Signals.Signal<boolean>) => {
	const el = $<HTMLInputElement>(id);
	signal.set(el.checked);
	el.addEventListener("change", () => signal.set(el.checked));
};

const resolutionEl = $<HTMLSelectElement>("resolution");
resolution.set(resolutionEl.value);
resolutionEl.addEventListener("input", () => resolution.set(resolutionEl.value));

bindOptionalNumber("framerate", framerate);
bindOptionalNumber("bitrate", bitrateKbps);
bindOptionalNumber("keyframe", keyframeMs);
bindNumber("volume", volume);
bindOptionalSelect("samplerate", sampleRate);
bindOptionalSelect("channels", channelCount);
bindOptionalNumber("opus-bitrate", opusBitrateKbps);
bindOptionalSelect("opus-frame-duration", opusFrameDuration);
bindOptionalNumber("opus-complexity", opusComplexity);
bindOptionalNumber("opus-plc", opusPacketLoss);
bindCheckbox("opus-fec", opusFec);
bindCheckbox("opus-dtx", opusDtx);

// Audio codec selector: drive the codec kind and show the matching options panel.
const audioCodecEl = $<HTMLSelectElement>("audio-codec");
const opusAdvancedEl = $("opus-advanced");
const syncAudioCodec = () => {
	audioCodecKind.set(audioCodecEl.value);
	opusAdvancedEl.hidden = audioCodecEl.value !== "opus";
};
audioCodecEl.addEventListener("change", syncAudioCodec);
syncAudioCodec();

// ---------------------------------------------------------------------------
// Codec menu - probe live support with WebCodecs
// ---------------------------------------------------------------------------

const CODECS: { label: string; value: string | undefined; probe?: string }[] = [
	{ label: "Auto", value: undefined },
	{ label: "H.264 (AVC, baseline)", value: "avc1.42E01F", probe: "avc1.42E01F" },
	{ label: "H.264 (AVC, high)", value: "avc1.640028", probe: "avc1.640028" },
	{ label: "VP8", value: "vp8", probe: "vp8" },
	{ label: "VP9", value: "vp09.00.10.08", probe: "vp09.00.10.08" },
	{ label: "AV1", value: "av01.0.04M.08", probe: "av01.0.04M.08" },
	{ label: "HEVC (H.265)", value: "hev1.1.6.L93.B0", probe: "hev1.1.6.L93.B0" },
];

async function buildCodecMenu() {
	const select = $<HTMLSelectElement>("codec");
	for (const entry of CODECS) {
		const option = document.createElement("option");
		option.value = entry.value ?? "auto";
		option.textContent = entry.label;

		if (entry.probe && "VideoEncoder" in globalThis) {
			try {
				const support = await VideoEncoder.isConfigSupported({
					codec: entry.probe,
					width: 1280,
					height: 720,
					bitrate: 2_000_000,
					framerate: 30,
				});
				if (!support.supported) {
					option.disabled = true;
					option.textContent += " - unsupported";
				}
			} catch {
				option.disabled = true;
				option.textContent += " - unsupported";
			}
		}
		select.appendChild(option);
	}

	select.addEventListener("change", () => {
		codec.set(select.value === "auto" ? undefined : select.value);
	});
}
buildCodecMenu();

// ---------------------------------------------------------------------------
// Negotiated values, shown inline beside each control once live
// ---------------------------------------------------------------------------

const setActual = (id: string, value: string | undefined) => {
	$(id).textContent = value ?? "";
};

// Video: the resolved encoder config (codec / resolution / fps / bitrate).
ui.run((effect) => {
	const v = effect.get(publish.broadcast.video.hd.resolved);
	setActual("codec-actual", v?.codec);
	setActual("resolution-actual", v?.width && v?.height ? `${v.width}×${v.height}` : undefined);
	setActual("framerate-actual", v?.framerate ? formatFps(v.framerate) : undefined);
	setActual("bitrate-actual", v?.bitrate ? formatBitrate(v.bitrate) : undefined);
	// The encoder doesn't report the negotiated keyframe interval, so show the
	// configured value (defaulting to the 2s encoder default) once live.
	const kf = effect.get(keyframeMs);
	setActual("keyframe-actual", v ? `${(kf ?? 2000) / 1000}s` : undefined);
});

// Gain is a local control (not negotiated), so just echo the current value.
ui.run((effect) => {
	setActual("volume-actual", `${effect.get(volume).toFixed(2)}×`);
});

// Audio: the resolved audio config (codec / sample rate / channels / bitrate).
ui.run((effect) => {
	const a = effect.get(publish.broadcast.audio.config);
	setActual("audiocodec-actual", a?.codec);
	setActual("samplerate-actual", a?.sampleRate ? `${a.sampleRate} Hz` : undefined);
	setActual("channels-actual", a?.numberOfChannels ? String(a.numberOfChannels) : undefined);
	setActual("opusbitrate-actual", a?.bitrate ? formatBitrate(a.bitrate) : undefined);
});

// ---------------------------------------------------------------------------
// Custom meta.json track
// ---------------------------------------------------------------------------
//
// A track-less Json.Producer retains the current value and fans it out to each
// subscriber, seeding late joiners. publishTrack registers it on the broadcast;
// the component's publish loop serves it whenever a viewer requests `meta.json`.
// We advertise the track in the catalog's `metadata` section (the hang catalog
// is a loose schema, so the extra key passes through and base consumers ignore
// it) so the watch inspector knows to subscribe.

const META_TRACK = "meta.json";

const meta = new Json.Producer<unknown>({
	initial: { title: "My Broadcast", location: "earth", note: "edit me" },
});

publish.broadcast.publishTrack(META_TRACK, (track, effect) => meta.serve(track, effect));
publish.broadcast.catalog.mutate((catalog) => {
	(catalog as typeof catalog & { metadata?: string[] }).metadata = [META_TRACK];
});

const metaTextEl = $<HTMLTextAreaElement>("metadata");
const metaBtn = $<HTMLButtonElement>("send-meta");

metaTextEl.addEventListener("input", () => {
	metaBtn.disabled = false;
});

metaBtn.addEventListener("click", () => {
	try {
		// update() emits a full snapshot first (seeding late joiners), then only
		// merge-patch deltas; a no-op if the value is unchanged.
		meta.update(JSON.parse(metaTextEl.value));
		metaTextEl.setCustomValidity("");
		metaBtn.disabled = true;
	} catch (err) {
		// Keep the button armed so the user can fix and retry.
		metaTextEl.setCustomValidity(`invalid JSON: ${(err as Error).message}`);
		metaTextEl.reportValidity();
	}
});

// ---------------------------------------------------------------------------
// Live graphs
// ---------------------------------------------------------------------------

const viz = new Signals.Effect();

const captureGraph = graph(viz, "Capture rate", { color: "#facc15", format: formatFps });
const uploadGraph = graph(viz, "Upload estimate", { color: "#34d399", format: formatBitrate });
const rttGraph = graph(viz, "Round trip", { color: "#38bdf8", format: (v) => `${Math.round(v)} ms` });
$("publish-graphs").append(captureGraph.el, uploadGraph.el, rttGraph.el);

// Count captured frames; the publish API has no encoded-frame counter, so this
// is the capture rate feeding the encoder (a good proxy for output fps).
let frames = 0;
viz.run((effect) => {
	if (effect.get(publish.broadcast.video.frame)) frames++;
});

let prevFrames = 0;
let prevWhen = performance.now();
viz.interval(() => {
	const now = performance.now();
	const elapsed = now - prevWhen;
	captureGraph.push(elapsed > 0 ? ((frames - prevFrames) * 1000) / elapsed : undefined);
	prevFrames = frames;
	prevWhen = now;

	const conn = publish.connection.established.peek();
	const up = conn?.sendBandwidth?.peek() as unknown as number | undefined;
	uploadGraph.push(up && up > 0 ? up : undefined);
	const rtt = conn?.rtt?.peek() as unknown as number | undefined;
	rttGraph.push(rtt && rtt > 0 ? rtt : undefined);
}, 250);

// Vite re-evaluates this module on hot reload, dropping the references to the
// module-scoped effects above. Close them on dispose so they don't get garbage
// collected unclosed (which the signals library warns about).
if (import.meta.hot) {
	import.meta.hot.dispose(() => {
		for (const effect of [ui, viz]) effect.close();
	});
}
