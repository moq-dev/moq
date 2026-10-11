/** Run the publisher backlog sweep inside Chromium, including a timer-starvation assertion. */
import { type Browser, chromium } from "playwright";

const build = await Bun.build({ entrypoints: [`${import.meta.dir}/serve-browser-entry.ts`], target: "browser" });
if (!build.success) throw new AggregateError(build.logs, "browser benchmark failed to bundle");
const script = await build.outputs[0].text();
const server = Bun.serve({
	port: 0,
	fetch: (request) =>
		new URL(request.url).pathname === "/bench.js"
			? new Response(script, { headers: { "Content-Type": "text/javascript" } })
			: new Response('<script type="module" src="/bench.js"></script>', {
					headers: { "Content-Type": "text/html" },
				}),
});
let browser: Browser | undefined;
try {
	browser = await chromium.launch({ channel: "chromium", headless: true });
	const page = await browser.newPage();
	page.on("pageerror", (error) => console.log("browser error:", error));
	await page.goto(`http://localhost:${server.port}`);
	await page.waitForFunction(() => "measure" in globalThis);
	console.log("protocol,groups,viewers,frames,ms,timer_ticks,max_timer_gap_ms,bytes");
	for (const protocol of ["lite", "ietf"])
		for (const groups of [32, 128])
			for (const viewers of [1, 8]) {
				const result = await page.evaluate(
					async ({ protocol, groups, viewers }) => {
						const run = (
							globalThis as unknown as {
								measure: (
									protocol: string,
									groups: number,
									viewers: number,
								) => Promise<{
									protocol: string;
									groups: number;
									viewers: number;
									frames: number;
									ms: number;
									ticks: number;
									maxGap: number;
									written: number;
								}>;
							}
						).measure;
						return run(protocol, groups, viewers);
					},
					{ protocol, groups, viewers },
				);
				console.log(
					`${protocol},${groups},${viewers},${result.frames},${result.ms.toFixed(2)},${result.ticks},${result.maxGap.toFixed(2)},${result.written}`,
				);
				if (!process.env.SERVE_BROWSER_CONTROL && result.ticks === 0)
					throw new Error(`${protocol} starved browser timers for the entire backlog`);
			}
} finally {
	server.stop(true);
	await browser?.close();
}
