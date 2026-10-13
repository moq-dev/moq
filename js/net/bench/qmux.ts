/** Compare eager and cold lazy fallback startup with real WebSocket negotiation. */
import { basename } from "node:path";
import { gzipSync } from "node:zlib";
import { type Browser, chromium } from "playwright";

const build = await Bun.build({
	entrypoints: [`${import.meta.dir}/qmux-eager.ts`, `${import.meta.dir}/qmux-lazy.ts`],
	target: "browser",
	splitting: true,
	minify: true,
});
if (!build.success) throw new AggregateError(build.logs, "qmux benchmark build failed");
const files = new Map(
	await Promise.all(
		build.outputs.map(async (file) => [basename(file.path), gzipSync(await file.arrayBuffer())] as const),
	),
);
const server = Bun.serve({
	port: 0,
	fetch(request, server) {
		const path = new URL(request.url).pathname;
		if (path === "/qmux") {
			if (server.upgrade(request, { headers: { "Sec-WebSocket-Protocol": "webtransport" } })) return;
			return new Response("upgrade required", { status: 400 });
		}
		const code = files.get(path.slice(1));
		if (code)
			return new Response(code, { headers: { "Content-Type": "text/javascript", "Content-Encoding": "gzip" } });
		const entry = path === "/lazy" ? "qmux-lazy.js" : "qmux-eager.js";
		return new Response(`<script type="module" src="/${entry}"></script>`, {
			headers: { "Content-Type": "text/html" },
		});
	},
	websocket: { message() {} },
});
let browser: Browser | undefined;
try {
	browser = await chromium.launch({ channel: "chromium", headless: true });
	console.log("round,latency_ms,variant,entry_gzip_bytes,connect_ms");
	for (let round = 1; round <= 3; round++) {
		for (const latency of [0, 50]) {
			for (const variant of round === 2 ? ["lazy", "eager"] : ["eager", "lazy"]) {
				const context = await browser.newContext();
				try {
					const page = await context.newPage();
					const cdp = await context.newCDPSession(page);
					await cdp.send("Network.enable");
					await cdp.send("Network.setCacheDisabled", { cacheDisabled: true });
					await cdp.send("Network.emulateNetworkConditions", {
						offline: false,
						latency,
						downloadThroughput: 1_250_000,
						uploadThroughput: 1_250_000,
					});
					await page.goto(`http://localhost:${server.port}/${variant}`);
					await page.waitForFunction(() => "measureQmux" in globalThis);
					const result = await page.evaluate(async (url) => {
						const bytes = performance
							.getEntriesByType("resource")
							.filter((entry) => entry.name.endsWith(".js"))
							.reduce((total, entry) => total + (entry as PerformanceResourceTiming).encodedBodySize, 0);
						const measure = (globalThis as unknown as { measureQmux(url: string): Promise<number> })
							.measureQmux;
						return { bytes, elapsed: await measure(url) };
					}, `http://localhost:${server.port}/qmux`);
					console.log(`${round},${latency},${variant},${result.bytes},${result.elapsed.toFixed(3)}`);
				} finally {
					await context.close();
				}
			}
		}
	}
} finally {
	server.stop(true);
	await browser?.close();
}
