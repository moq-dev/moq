// Role logic for the browser client: read ?role= and wire up a publisher, the real
// <moq-watch-ui> player, or a refused session. The Playwright drivers (driver.ts for the interop
// matrix, media.ts for the media-output and lifecycle checks, close.ts for the close code) poll the
// state each role reports over the console or mirrors onto the DOM.
import type MoqPublish from "@moq/publish/element";
import type MoqWatch from "@moq/watch/element";
import { refused, refusedWebSocket } from "./close";
import {
	AUDIO_CODECS,
	type AudioCodec,
	CAPTURE_EVENT,
	type CaptureState,
	FAULTS,
	type Fault,
	publish,
	SAMPLE_MS,
} from "./contract";
import { finish } from "./finish";
import { Fixture } from "./fixture";
import { attach, watchResources } from "./probe";

const params = new URLSearchParams(location.search);
const role = params.get("role");
const url = params.get("url") ?? "";
const broadcast = params.get("broadcast") ?? "";
// The versions the element offers, or its defaults when absent.
const versions = params.get("versions");

function parseFault(value: string | null): Fault {
	if (value === null) return "none";
	const fault = FAULTS.find((f) => f === value);
	if (!fault) throw new Error(`unknown fault: ${value}`);
	return fault;
}

if (role === "finish-publish" || role === "finish-subscribe") {
	finish(role, url, broadcast, versions, Number(params.get("frames") ?? 10), Number(params.get("timeout") ?? 60_000));
} else if (role === "publish") {
	const el = document.createElement("moq-publish") as MoqPublish;
	el.setAttribute("url", url);
	if (versions) el.setAttribute("versions", versions);
	el.setAttribute("name", broadcast);
	// Media is encoded lazily when a player asks.
	const media = params.get("media");
	if (media) {
		// The interop driver's file of the picture and test tone, decoded and looped rather than
		// captured: a fake device splices the tone whenever the machine is short of CPU.
		const response = await fetch(media);
		if (!response.ok) throw new Error(`no publisher media at ${media}: ${response.status}`);
		el.controls.source.set(new File([await response.blob()], "interop-media"));
	} else {
		// Chromium's --use-fake-device-for-media-stream feeds getUserMedia fake camera and microphone
		// input, for the capture checks in media.ts.
		el.setAttribute("source", "camera");
		// Keep Chromium's voice processing off the microphone: that mixes stereo to mono and
		// suppresses a steady tone as noise.
		el.signals.run((effect) => {
			const mic = effect.get(el.sources.audio);
			if (mic && "constraints" in mic) {
				mic.constraints.set({
					channelCount: 2,
					echoCancellation: false,
					noiseSuppression: false,
					autoGainControl: false,
				});
			}
		});
	}
	document.body.appendChild(el);
	// So the driver can wait for the session to close before closing the browser.
	watchResources();
	// Report the public source state the way players report samples. See CAPTURE_EVENT.
	const sample = () => {
		type CaptureSource = { out: { source: { peek(): unknown }; error?: { peek(): Error | undefined } } };
		const video = el.sources.video.peek() as CaptureSource | undefined;
		const audio = el.sources.audio.peek() as CaptureSource | undefined;
		const state: CaptureState = {
			videoError: video?.out.error?.peek()?.name,
			audioError: audio?.out.error?.peek()?.name,
			videoActive: video?.out.source.peek() !== undefined,
			audioActive: audio?.out.source.peek() !== undefined,
		};
		console.debug(CAPTURE_EVENT + JSON.stringify(state));
	};
	sample();
	self.setInterval(sample, SAMPLE_MS);
} else if (role === "fixture") {
	// The deterministic publisher. Needs no camera, no microphone, and no permissive launch flags:
	// the picture and the tone are generated in the page. See fixture.ts.
	const host = document.createElement("div");
	host.id = "fixture";
	document.body.appendChild(host);

	const fault = parseFault(params.get("fault"));
	const audioCodec = params.get("audio-codec") ?? "opus";
	if (!AUDIO_CODECS.some((codec) => codec === audioCodec)) throw new Error(`unknown audio codec: ${audioCodec}`);
	const props = {
		url,
		name: broadcast,
		fault,
		versions: versions?.split(","),
		generation: Number(params.get("generation") ?? 0),
		audioCodec: audioCodec as AudioCodec,
	};
	let fixture: Fixture | undefined = new Fixture(host, props);

	// The driver stops and restarts the publisher in place to exercise a same-path republish, which
	// has to reuse this page so the audio context keeps its user activation.
	publish({
		stop: () => {
			fixture?.close();
			fixture = undefined;
		},
		start: () => {
			fixture?.close();
			fixture = new Fixture(host, props);
		},
		disableVideo: () => fixture?.setVideo(false),
		enableVideo: () => fixture?.setVideo(true),
		changeVideoCodec: () => fixture?.changeVideoCodec(),
		resizeVideo: () => fixture?.resizeVideo(),
		changeAudioCodec: () => fixture?.changeAudioCodec(),
		changeAudioRate: () => fixture?.changeAudioRate(),
		changeAudioChannels: () => fixture?.changeAudioChannels(),
		addAudioRendition: () => fixture?.addAudioRendition(),
		removeAudioRendition: () => fixture?.removeAudioRendition(),
	});
} else if (role === "subscribe") {
	await customElements.whenDefined("moq-watch");
	const el = document.createElement("moq-watch") as MoqWatch;
	el.setAttribute("url", url);
	if (versions) el.setAttribute("versions", versions);
	el.setAttribute("name", broadcast);
	if (params.get("muted") === "true") el.setAttribute("muted", "");
	// A render target is what makes <moq-watch> actually subscribe to and decode
	// the video track. @moq/publish only encodes on subscriber demand, so without
	// this the publisher never produces frames.
	el.appendChild(document.createElement("canvas"));

	// The media driver runs the player in a background window, where the default visibility policy
	// stops downloading and leaves the canvas black. Only it passes this.
	const visible = params.get("visible");
	if (visible) el.setAttribute("visible", visible);

	// Only the late-join negative control passes this, to hold the player behind live.
	const delay = params.get("delay");
	if (delay) el.setAttribute("delay", delay);

	// The interop matrix checks each audio rendition in a page of its own, pinned by codec (the
	// catalog's up to its first `.`). The target lands with the catalog, before the player has
	// probed which renditions it can decode, so no other rendition ever plays here.
	const codec = params.get("codec");
	if (codec) {
		el.signals.run((effect) => {
			const renditions = effect.get(el.broadcast.out.catalog)?.audio?.renditions ?? {};
			const name = Object.keys(renditions).find((name) => renditions[name].codec.split(".")[0] === codec);
			if (name) el.controls.audio.target.set({ name });
		});
	}

	const player = document.createElement("moq-watch-ui");
	player.appendChild(el);
	document.body.appendChild(player);

	watchResources();
	// The interop matrix names the tone each channel should carry; the media checks do not.
	const tones = (params.get("tone") ?? "").split(",").filter(Boolean).map(Number);
	let stop = attach(el, tones);

	// Where the leaked-session control parks the player it refuses to tear down.
	const leak = document.createElement("div");
	leak.hidden = true;
	document.body.appendChild(leak);

	publish({
		detach: () => {
			stop();
			el.remove();
		},
		startLeak: () => {
			// Stand up a second player on the same broadcast and leave it connected. The driver waits
			// for that player to start before detaching the real one, so a zero-resource instant cannot
			// satisfy the negative control before the leak has started.
			const stray = document.createElement("moq-watch") as MoqWatch;
			stray.setAttribute("url", url);
			stray.setAttribute("name", broadcast);
			stray.setAttribute("visible", "always");
			stray.appendChild(document.createElement("canvas"));
			leak.appendChild(stray);
		},
		reattach: () => {
			stop();
			// The torn-down player leaves its last frame on the canvas, which can be newer than the frame
			// the driver read before detaching. Blank it so every frame read after this was presented by
			// the new session, not left over from the old one.
			const canvas = el.querySelector("canvas");
			canvas?.getContext("2d")?.clearRect(0, 0, canvas.width, canvas.height);
			player.appendChild(el);
			stop = attach(el, tones);
		},
	});
} else if (role === "close") {
	// The relay refuses this session; mirror how it closed for close.ts to check.
	document.body.dataset.interopClose = JSON.stringify(
		await (params.get("transport") === "websocket" ? refusedWebSocket(url) : refused(url)),
	);
} else {
	throw new Error("missing ?role=publish|fixture|subscribe|close");
}
