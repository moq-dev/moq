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
import { type Audio, Json, Net, Signals, Source, type Video } from "@moq/publish";
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

// The component builds its encoders in the constructor, so they're ready as soon as
// the element upgrades. `publish.video`/`publish.audio` are the encoders whose signals
// we drive below.
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

// Mic processing constraints; undefined means "browser default". macOS defaults these on, which
// engages voice processing: auto gain can slowly pull the level down and other audio gets ducked.
const echoCancellation = new Signals.Signal<boolean | undefined>(undefined);
const autoGainControl = new Signals.Signal<boolean | undefined>(undefined);
const noiseSuppression = new Signals.Signal<boolean | undefined>(undefined);

// Opus-specific knobs (the "Opus options" panel), mapping 1:1 onto OpusConfig.
const opusBitrateKbps = new Signals.Signal<number | undefined>(undefined);
const opusFrameDuration = new Signals.Signal<number | undefined>(undefined); // ms (2.5 to 60)
const opusComplexity = new Signals.Signal<number | undefined>(undefined); // 0 (fast) … 10 (best)
const opusFec = new Signals.Signal(false); // in-band forward error correction
const opusPacketLoss = new Signals.Signal<number | undefined>(undefined); // expected loss %
const opusDtx = new Signals.Signal(false); // discontinuous transmission (silence)

const ui = new Signals.Effect();

type VideoTarget = {
	width?: number;
	height?: number;
	frameRate?: number;
};

function readVideoTarget(effect: Signals.Effect): VideoTarget {
	const res = effect.get(resolution);
	const [rawWidth, rawHeight] = res ? res.split("x").map(Number) : [undefined, undefined];
	return {
		width: rawWidth && Number.isFinite(rawWidth) ? rawWidth : undefined,
		height: rawHeight && Number.isFinite(rawHeight) ? rawHeight : undefined,
		frameRate: effect.get(framerate),
	};
}

function cameraConstraints(target: VideoTarget): Video.Constraints | undefined {
	const constraints: Video.Constraints = {};
	if (target.width !== undefined) constraints.width = { ideal: target.width, max: target.width };
	if (target.height !== undefined) constraints.height = { ideal: target.height, max: target.height };
	if (target.frameRate !== undefined) constraints.frameRate = { ideal: target.frameRate, max: target.frameRate };

	return Object.keys(constraints).length ? constraints : undefined;
}

function encoderConfig(effect: Signals.Effect, target: VideoTarget): Video.Config {
	const br = effect.get(bitrateKbps);
	const kf = effect.get(keyframeMs);
	return {
		codec: effect.get(codec),
		maxPixels: target.width && target.height ? target.width * target.height : undefined,
		maxBitrate: br != null ? br * 1000 : undefined,
		keyframeInterval: kf != null ? Net.Time.Milli(kf) : undefined,
		frameRate: target.frameRate,
	};
}

// Compose the WebCodecs/MoQ video encoder config and push it onto the HD
// rendition. Undefined fields are omitted, so the encoder auto-sizes them.
ui.run((effect) => {
	publish.video.config.set(encoderConfig(effect, readVideoTarget(effect)));
});

// Request the selected resolution from the camera itself, not just cap the encoder.
// publish.sources.video holds the active Camera source (undefined for screen/file); its constraints
// re-acquire the track on change. getUserMedia uses `ideal`, so a camera that can't reach the target
// falls back to its best (the green "actual" readout shows what it gave).
ui.run((effect) => {
	const source = effect.get(publish.sources.video);
	if (!(source instanceof Source.Camera)) return;

	effect.set(source.constraints, cameraConstraints(readVideoTarget(effect)));
});

// Audio general settings (volume gain, output sample rate, channel mix).
ui.run((effect) => {
	publish.audio.volume.set(effect.get(volume));
	publish.audio.sampleRate.set(effect.get(sampleRate));
	publish.audio.channelCount.set(effect.get(channelCount));
});

// Mic processing constraints go to the capture itself (getUserMedia re-acquires the track on
// change). publish.sources.audio holds the active Microphone source (undefined for screen/file).
ui.run((effect) => {
	const source = effect.get(publish.sources.audio);
	if (!source || !("constraints" in source)) return;

	const constraints = {
		echoCancellation: effect.get(echoCancellation),
		autoGainControl: effect.get(autoGainControl),
		noiseSuppression: effect.get(noiseSuppression),
	};
	const any = Object.values(constraints).some((v) => v !== undefined);
	source.constraints.set(any ? constraints : undefined);
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
		...(frameDuration != null ? { frameDuration: Net.Time.Milli(frameDuration) } : {}),
		...(complexity != null ? { complexity } : {}),
		...(packetLoss != null ? { packetlossperc: packetLoss } : {}),
		useinbandfec: effect.get(opusFec),
		usedtx: effect.get(opusDtx),
	};
	publish.audio.codec.set(config);
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

// An optional on/off select where the empty value ("Auto") means undefined (browser default).
const bindOptionalBoolean = (id: string, signal: Signals.Signal<boolean | undefined>) => {
	const el = $<HTMLSelectElement>(id);
	const sync = () => signal.set(el.value === "" ? undefined : el.value === "on");
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
bindOptionalBoolean("echo-cancellation", echoCancellation);
bindOptionalBoolean("auto-gain-control", autoGainControl);
bindOptionalBoolean("noise-suppression", noiseSuppression);
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
void buildCodecMenu();

// ---------------------------------------------------------------------------
// Negotiated values, shown inline beside each control once live
// ---------------------------------------------------------------------------

const setActual = (id: string, value: string | undefined) => {
	$(id).textContent = value ?? "";
};

// Video: the resolved encoder config (codec / resolution / fps / bitrate).
ui.run((effect) => {
	const v = effect.get(publish.video.out.resolved);
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

// Report the transport negotiated by the live connection.
ui.run((effect) => {
	const conn = effect.get(publish.connection.established);
	$("network-transport").textContent = conn ? (conn.transport === "websocket" ? "WebSocket" : "WebTransport") : "";
});

// Audio: the resolved audio config (codec / sample rate / channels / bitrate).
ui.run((effect) => {
	const a = effect.get(publish.audio.out.catalog);
	setActual("audiocodec-actual", a?.codec);
	setActual("samplerate-actual", a?.sampleRate ? `${a.sampleRate} Hz` : undefined);
	setActual("channels-actual", a?.numberOfChannels ? String(a.numberOfChannels) : undefined);
	setActual("opusbitrate-actual", a?.bitrate ? formatBitrate(a.bitrate) : undefined);
});

// ---------------------------------------------------------------------------
// Custom metadata carried within the broadcast
// ---------------------------------------------------------------------------
//
// We serve the metadata as a separate `meta.json` track *within* the broadcast,
// using `broadcast.net` (the underlying producer the element exposes). `net` is
// recreated on each (re)connection, so an effect (re)creates the track and seeds
// it with the latest value; a long cache window lets a late viewer replay the
// most recent snapshot. The track is advertised in the catalog's `metadata` list
// so the watch side knows to subscribe.
const META_TRACK = "meta.json";

// The latest metadata, retained across reconnects so each fresh track is seeded with it.
let currentMeta: unknown = { title: "My Broadcast", location: "earth", note: "edit me" };
let activeMeta: Json.Snapshot.Producer<unknown> | undefined;

const setMeta = (value: unknown) => {
	currentMeta = value;
	activeMeta?.update(value);
};

const meta = new Signals.Effect();

meta.run((effect) => {
	const net = effect.get(publish.broadcast.net);
	if (!net) return;

	// A day-long cache so a viewer joining long after the last edit still replays the value.
	const track = net.createTrack(META_TRACK, { latencyMax: 86_400_000 });
	effect.cleanup(() => track.close());

	const producer = new Json.Snapshot.Producer<unknown>(track);
	producer.update(currentMeta);
	activeMeta = producer;
	effect.cleanup(() => {
		if (activeMeta === producer) activeMeta = undefined;
	});
});

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
		// Publishes a fresh snapshot on the meta.json track (a no-op if unchanged); the
		// cache window seeds late joiners.
		setMeta(JSON.parse(metaTextEl.value));
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
	if (effect.get(publish.capture.out.frame)) frames++;
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
		for (const effect of [ui, meta, viz]) effect.close();
	});
}
