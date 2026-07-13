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
} else {
	throw new Error("missing ?role=publish|subscribe");
}
