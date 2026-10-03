/**
 * The publisher page for the microphone trace: the demo's `<moq-publish>` capturing audio only.
 *
 * Whatever device Chromium offers is captured through the element's real path (getUserMedia, the
 * capture worklet, WebCodecs Opus), so the trace carries a browser publisher's own cadence. Under
 * `record.ts` that device is Chromium's fake one, a tone.
 *
 *     ?url=https://cdn.moq.dev/anon&broadcast=aq-mic-1234.hang
 *
 * @module
 */
import "@moq/publish/element";

const params = new URLSearchParams(location.search);
const required = (name: string): string => {
	const value = params.get(name);
	if (!value) throw new Error(`missing ?${name}`);
	return value;
};

const publish = document.createElement("moq-publish");
publish.setAttribute("url", required("url"));
publish.setAttribute("name", required("broadcast"));
publish.setAttribute("source", "camera");
publish.setAttribute("invisible", "");
publish.setAttribute("preview", "none");
document.body.appendChild(publish);
