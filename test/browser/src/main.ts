// Standalone page for the browser smoke client. Registers the <moq-publish> /
// <moq-watch> elements (the public API) and configures one based on ?role=.
// The Playwright driver polls the watch element's decoded-frame stats signal.
import "@moq/publish/element";
import "@moq/watch/element";

const params = new URLSearchParams(location.search);
const role = params.get("role");
const url = params.get("url") ?? "";
const broadcast = params.get("broadcast") ?? "";

if (role === "publish") {
	const el = document.createElement("moq-publish");
	el.setAttribute("url", url);
	el.setAttribute("name", broadcast);
	el.setAttribute("muted", ""); // video only; keep the audio encoder out of it
	// Chromium's --use-fake-device-for-media-stream feeds getUserMedia a pattern.
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
	document.body.appendChild(el);
} else {
	throw new Error("missing ?role=publish|subscribe");
}
