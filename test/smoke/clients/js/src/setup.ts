// Role logic for the browser client: read ?role= and wire up a publisher or the real
// <moq-watch-ui> player. The Playwright drivers (driver.ts for the interop matrix, media.ts for the
// media-output and lifecycle checks) poll the state each role mirrors onto the DOM.
import type MoqPublish from "@moq/publish/element";
import type MoqWatch from "@moq/watch/element";
import { FAULTS, type Fault, publish } from "./contract";
import { Fixture } from "./fixture";
import { attach, watchResources } from "./probe";

const params = new URLSearchParams(location.search);
const role = params.get("role");
const url = params.get("url") ?? "";
const broadcast = params.get("broadcast") ?? "";

function parseFault(value: string | null): Fault {
	if (value === null) return "none";
	const fault = FAULTS.find((f) => f === value);
	if (!fault) throw new Error(`unknown fault: ${value}`);
	return fault;
}

if (role === "publish") {
	const el = document.createElement("moq-publish") as MoqPublish;
	el.setAttribute("url", url);
	el.setAttribute("name", broadcast);
	// Chromium's --use-fake-device-for-media-stream feeds getUserMedia fake
	// camera and microphone input. Audio is encoded lazily when a player asks.
	el.setAttribute("source", "camera");
	document.body.appendChild(el);
} else if (role === "fixture") {
	// The deterministic publisher. Needs no camera, no microphone, and no permissive launch flags:
	// the picture and the tone are generated in the page. See fixture.ts.
	const host = document.createElement("div");
	host.id = "fixture";
	document.body.appendChild(host);

	const fault = parseFault(params.get("fault"));
	let fixture: Fixture | undefined = new Fixture(host, url, broadcast, fault);

	// The driver stops and restarts the publisher in place to exercise a same-path republish, which
	// has to reuse this page so the audio context keeps its user activation.
	publish({
		stop: () => {
			fixture?.close();
			fixture = undefined;
		},
		start: () => {
			fixture?.close();
			fixture = new Fixture(host, url, broadcast, fault);
		},
	});
} else if (role === "subscribe") {
	await customElements.whenDefined("moq-watch");
	const el = document.createElement("moq-watch") as MoqWatch;
	el.setAttribute("url", url);
	el.setAttribute("name", broadcast);
	// A render target is what makes <moq-watch> actually subscribe to and decode
	// the video track. @moq/publish only encodes on subscriber demand, so without
	// this the publisher never produces frames.
	el.appendChild(document.createElement("canvas"));

	// The media driver runs the player in a background window, where the default visibility policy
	// stops downloading and leaves the canvas black. Only it passes this.
	const visible = params.get("visible");
	if (visible) el.setAttribute("visible", visible);

	const player = document.createElement("moq-watch-ui");
	player.appendChild(el);
	document.body.appendChild(player);

	watchResources();
	let stop = attach(el);

	publish({
		detach: () => {
			stop();
			el.remove();
		},
		detachLeaky: () => {
			// Inject the application defect directly: the detach command forgets to remove the
			// already-active player, so its established session and media graph remain observable.
			stop();
		},
		reattach: () => {
			stop();
			player.appendChild(el);
			stop = attach(el);
		},
	});
} else {
	throw new Error("missing ?role=publish|fixture|subscribe");
}
