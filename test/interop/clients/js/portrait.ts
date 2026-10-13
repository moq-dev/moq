import { parseArgs } from "node:util";
import type MoqWatch from "@moq/watch/element";
import { finishTraces, launch, open, pageUrl, serve, throwPageErrors } from "./harness";

const { values } = parseArgs({ options: { url: { type: "string" } } });
if (!values.url) throw new Error("portrait.ts requires --url");
const server = serve();
const browser = await launch();
let failed = true;
try {
	const [page, errors] = await open(
		browser,
		pageUrl(server.origin, "subscribe", {
			url: values.url,
			broadcast: "portrait.hang/transcode.hang",
			muted: "true",
			visible: "always",
		}),
		"portrait",
		true,
	);
	await page.evaluate(() => {
		const watch = document.querySelector("moq-watch") as MoqWatch;
		watch.controls.target.set({ name: "video/360p" });
	});
	// The real renderer's frame and clock prove the rung was downloaded, decoded and painted.
	await page.waitForFunction(
		() => {
			const watch = document.querySelector("moq-watch") as MoqWatch;
			const frame = watch.renderer.out.frame.peek();
			return (
				watch.video.source.out.track.peek() === "video/360p" &&
				frame?.displayWidth === 360 &&
				frame.displayHeight === 640 &&
				(watch.renderer.out.timestamp.peek() ?? 0) > 1000
			);
		},
		undefined,
		{ timeout: 30_000 },
	);
	throwPageErrors(errors);
	console.log("portrait publish -> transcode -> moq-watch: 360x640 rung painted");
	failed = false;
} finally {
	await finishTraces(failed);
	await browser.close();
	server.stop();
}
