/**
 * MoQ publish demo built on the <moq-publish-ui> web component.
 *
 * The component owns capture (camera / screen / file / mic), preview, go-live,
 * and mute. This demo adds on top of it:
 *
 *   1. A side panel of *encoder* settings. The component publishes with sensible
 *      defaults; here we drive the broadcast's encoder signals directly so codec,
 *      bitrate, resolution, frame rate, and the full Opus config are editable.
 *   2. A custom `meta.json` track carried *within* the broadcast.
 *   3. A "Negotiated" panel: the resolved encode config plus live graphs (capture
 *      rate, upload-bandwidth estimate, round trip). The publish API exposes no
 *      encoded-byte counter, so these are the honestly-observable signals.
 */

import "./highlight";
import "@moq/publish/element"; // defines <moq-publish>
import "@moq/publish/ui"; // defines <moq-publish-ui>
import { type Audio, Json, type Net, Signals } from "@moq/publish";
import type MoqPublish from "@moq/publish/element";
import MoqPublishSupport from "@moq/publish/support/element";
import { formatBitrate, formatFps, graph, renderRows } from "./viz";

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

// ---------------------------------------------------------------------------
// Encoder settings - reactive Signals the broadcast's encoders subscribe to.
// ---------------------------------------------------------------------------

// Video encode (WebCodecs / MoQ)
const codec = new Signals.Signal<string | undefined>(undefined); // undefined => encoder picks
const resolution = new Signals.Signal("1280x720");
const framerate = new Signals.Signal(30);
const bitrateKbps = new Signals.Signal(2000);
const keyframeMs = new Signals.Signal(2000);

// Audio encode. Only Opus exists today, but everything downstream keys off this.
const audioCodecKind = new Signals.Signal("opus");
const volume = new Signals.Signal(1);
const sampleRate = new Signals.Signal<number | undefined>(48000);
const channelCount = new Signals.Signal<number | undefined>(2);

// Opus-specific knobs (the "Opus options" panel), mapping 1:1 onto OpusConfig.
const opusBitrateKbps = new Signals.Signal(64);
const opusFrameDuration = new Signals.Signal(20); // ms (Opus supports 2.5 to 60 ms)
const opusComplexity = new Signals.Signal(10); // 0 (fast) … 10 (best quality)
const opusFec = new Signals.Signal(false); // in-band forward error correction
const opusPacketLoss = new Signals.Signal(0); // expected loss %, tunes FEC strength
const opusDtx = new Signals.Signal(false); // discontinuous transmission (silence)

const ui = new Signals.Effect();

// Compose the WebCodecs/MoQ video encoder config and push it onto the HD
// rendition. Resolution caps the encoded pixels; the rest map straight through.
ui.run((effect) => {
	const [w, h] = effect.get(resolution).split("x").map(Number);
	publish.broadcast.video.hd.config.set({
		codec: effect.get(codec),
		maxPixels: (w ?? 1280) * (h ?? 720),
		maxBitrate: effect.get(bitrateKbps) * 1000,
		keyframeInterval: effect.get(keyframeMs) as Net.Time.Milli,
		frameRate: effect.get(framerate),
	});
});

// Audio general settings (volume gain, output sample rate, channel mix).
ui.run((effect) => {
	publish.broadcast.audio.volume.set(effect.get(volume));
	publish.broadcast.audio.sampleRate.set(effect.get(sampleRate));
	publish.broadcast.audio.channelCount.set(effect.get(channelCount));
});

// Compose the structured audio codec config; today only Opus.
ui.run((effect) => {
	if (effect.get(audioCodecKind) === "opus") {
		const config: Audio.OpusConfig = {
			mime: "opus",
			bitrate: effect.get(opusBitrateKbps) * 1000,
			frameDuration: effect.get(opusFrameDuration),
			complexity: effect.get(opusComplexity),
			useinbandfec: effect.get(opusFec),
			packetlossperc: effect.get(opusPacketLoss),
			usedtx: effect.get(opusDtx),
		};
		publish.broadcast.audio.codec.set(config);
	}
});

// ---------------------------------------------------------------------------
// Input bindings (DOM -> Signal)
// ---------------------------------------------------------------------------

const bindNumber = (id: string, signal: Signals.Signal<number>) => {
	const el = $<HTMLInputElement | HTMLSelectElement>(id);
	signal.set(Number(el.value));
	el.addEventListener("input", () => signal.set(Number(el.value)));
};
const bindCheckbox = (id: string, signal: Signals.Signal<boolean>) => {
	const el = $<HTMLInputElement>(id);
	signal.set(el.checked);
	el.addEventListener("change", () => signal.set(el.checked));
};

const resolutionEl = $<HTMLSelectElement>("resolution");
resolution.set(resolutionEl.value);
resolutionEl.addEventListener("input", () => resolution.set(resolutionEl.value));

bindNumber("framerate", framerate);
bindNumber("bitrate", bitrateKbps);
bindNumber("keyframe", keyframeMs);
bindNumber("volume", volume);
bindNumber("opus-bitrate", opusBitrateKbps);
bindNumber("opus-frame-duration", opusFrameDuration);
bindNumber("opus-complexity", opusComplexity);
bindCheckbox("opus-fec", opusFec);
bindNumber("opus-plc", opusPacketLoss);
bindCheckbox("opus-dtx", opusDtx);

// sample rate / channels are numbers; bind via change to keep the defaults.
const sampleRateEl = $<HTMLSelectElement>("samplerate");
sampleRateEl.addEventListener("change", () => sampleRate.set(Number(sampleRateEl.value)));
const channelsEl = $<HTMLSelectElement>("channels");
channelsEl.addEventListener("change", () => channelCount.set(Number(channelsEl.value)));

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
	{ label: "Auto (encoder picks best)", value: undefined },
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
// Negotiated config + live graphs
// ---------------------------------------------------------------------------

// The resolved encoder config (codec/resolution/bitrate/fps that were actually
// negotiated) only exists once a source is live, so the panel hides until then.
ui.run((effect) => {
	const v = effect.get(publish.broadcast.video.hd.resolved);
	const a = effect.get(publish.broadcast.audio.config);
	const section = $("negotiated-section");
	if (!v && !a) {
		section.hidden = true;
		return;
	}
	section.hidden = false;
	renderRows($("negotiated-info"), [
		["video codec", v?.codec],
		["resolution", v?.width && v?.height ? `${v.width}×${v.height}` : undefined],
		["frame rate", v?.framerate ? `${v.framerate} fps` : undefined],
		["video bitrate", v?.bitrate ? formatBitrate(v.bitrate) : undefined],
		["audio codec", a?.codec],
		["sample rate", a?.sampleRate ? `${a.sampleRate} Hz` : undefined],
		["channels", a?.numberOfChannels ? String(a.numberOfChannels) : undefined],
		["audio bitrate", a?.bitrate ? formatBitrate(a.bitrate) : undefined],
	]);
});

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
