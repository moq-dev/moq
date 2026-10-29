/** Measure idle encoding and late-join catch-up in native Chromium, sweeping tracks and retention. */
import { type Browser, chromium } from "playwright";

const build = await Bun.build({ entrypoints: [`${import.meta.dir}/audio-page.ts`], target: "browser" });
if (!build.success) throw new AggregateError(build.logs, "audio benchmark failed to bundle");
const script = await build.outputs[0].text();
const server = Bun.serve({
	port: 0,
	hostname: "127.0.0.1",
	fetch(request) {
		if (new URL(request.url).pathname === "/audio.js")
			return new Response(script, { headers: { "Content-Type": "text/javascript" } });
		return new Response('<script type="module" src="/audio.js"></script>', {
			headers: { "Content-Type": "text/html" },
		});
	},
});
let browser: Browser | undefined;
try {
	browser = await chromium.launch({ headless: true, executablePath: process.env.BROWSER_BIN });
	const page = await browser.newPage();
	page.on("pageerror", (error) => console.error(error));
	await page.goto(`http://127.0.0.1:${server.port}/`);
	await page.waitForFunction(() => "measure" in globalThis);
	console.log("tracks,retention,policy,idle_ms,catchup_ms");
	for (const tracks of [1, 8])
		for (const seconds of [1, 30])
			for (const onDemand of [false, true]) {
				const result = await page.evaluate(
					async ({ tracks, seconds, onDemand }) => {
						return (
							globalThis as unknown as {
								measure(
									tracks: number,
									seconds: number,
									onDemand: boolean,
								): Promise<{ idle: number; catchup: number }>;
							}
						).measure(tracks, seconds, onDemand);
					},
					{ tracks, seconds, onDemand },
				);
				console.log(
					`${tracks},${seconds}s,${onDemand ? "buffered" : "always"},${result.idle.toFixed(2)},${result.catchup.toFixed(2)}`,
				);
			}
} finally {
	await browser?.close();
	server.stop(true);
}
