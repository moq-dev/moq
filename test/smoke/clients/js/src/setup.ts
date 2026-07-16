// Role logic for the browser client: read ?role= and wire up a <moq-publish> or
// the real <moq-watch-ui> player. The Playwright driver (driver.ts) checks
// rendered playback and drives the player's controls.
const params = new URLSearchParams(location.search);
const role = params.get("role");
const url = params.get("url") ?? "";
const broadcast = params.get("broadcast") ?? "";

if (role === "publish") {
	const el = document.createElement("moq-publish");
	el.setAttribute("url", url);
	el.setAttribute("name", broadcast);
	// Chromium's --use-fake-device-for-media-stream feeds getUserMedia fake
	// camera and microphone input. Audio is encoded lazily when a player asks.
	el.setAttribute("source", "camera");
	document.body.appendChild(el);
} else if (role === "subscribe") {
	await customElements.whenDefined("moq-watch");
	const el = document.createElement("moq-watch");
	el.setAttribute("url", url);
	el.setAttribute("name", broadcast);
	// A render target is what makes <moq-watch> actually subscribe to and decode
	// the video track. @moq/publish only encodes on subscriber demand, so without
	// this the publisher never produces frames.
	el.appendChild(document.createElement("canvas"));

	const player = document.createElement("moq-watch-ui");
	player.appendChild(el);
	document.body.appendChild(player);

	// Playwright may evaluate in a different JavaScript world, where DOM nodes
	// are shared but custom-element instance fields are not. Mirror the state the
	// driver needs through data attributes, which are visible in every world.
	const syncState = () => {
		el.dataset.smokeVideoFrames = String(el.video.out.stats.peek()?.frameCount ?? 0);
		el.dataset.smokeVideoTimestamp = String(el.video.out.timestamp.peek() ?? "");
		el.dataset.smokeAudioBytes = String(el.audio.out.stats.peek()?.bytesReceived ?? 0);
		el.dataset.smokeAudioContext = el.audio.out.context.peek()?.state ?? "";
		el.dataset.smokeHasAudio = String(el.catalog?.audio !== undefined);
		el.dataset.smokePaused = String(el.paused);
		el.dataset.smokeReady = "";
	};
	syncState();
	const stateTimer = window.setInterval(syncState, 50);
	window.addEventListener("pagehide", () => window.clearInterval(stateTimer), { once: true });
} else {
	throw new Error("missing ?role=publish|subscribe");
}

export {};
