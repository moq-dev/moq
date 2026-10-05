/**
 * The page under measurement: the demo's player and nothing else.
 *
 * The element and its registration imports are the ones `demo/web` uses, so a regression in the
 * element's attribute handling or the worklet reaches these numbers the way it reaches a viewer.
 * Everything comes from the query string, so one build serves every row:
 *
 *     ?url=http://127.0.0.1:4499&broadcast=tone-opus.hang&delay=auto
 *
 * The driver drains the probe through `globalThis.audioQuality`.
 *
 * @module
 */
import "@moq/watch/element";
import type MoqWatch from "@moq/watch/element";
import { type Probe, probe } from "./probe.ts";

const params = new URLSearchParams(location.search);
const required = (name: string): string => {
	const value = params.get(name);
	if (!value) throw new Error(`missing ?${name}`);
	return value;
};

const watch = document.createElement("moq-watch") as MoqWatch;
watch.setAttribute("url", required("url"));
watch.setAttribute("name", required("broadcast"));
// "auto" adapts, and is what every profile but the control runs. A fixed delay carries its unit:
// the element rejects `delay="250"`.
watch.setAttribute("delay", params.get("delay") ?? "auto");
watch.appendChild(document.createElement("canvas"));
document.body.appendChild(watch);

// Audio is the measurement: muted, nothing would reach the ring. Volume first, because un-muting
// restores the stashed volume.
watch.volume = 1;
watch.muted = false;
watch.paused = false;

declare global {
	var audioQuality: Probe;
}
globalThis.audioQuality = probe(watch, params.get("capture") === "true");
