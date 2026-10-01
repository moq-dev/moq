/**
 * Plays one matrix row in headless Chromium, and refuses a row whose numbers would mean something
 * other than what its key says.
 *
 * The page dials the shaper, which forwards UDP and nothing else. So the page's fetch of the
 * certificate hash is answered here with the relay's own (the certificate is pinned by hash, so
 * dialing another port needs nothing more), and a WebSocket fallback finds nobody listening: the
 * session is WebTransport through the shaper, or nothing.
 *
 *     bun driver.ts --url http://127.0.0.1:4501 --fingerprint http://127.0.0.1:4500/certificate.sha256 \
 *         --broadcast tone-opus.hang --page dist --ring plain --delay auto --duration 60 \
 *         --tag chromium-opus-48000-mild-plain --out <run dir>
 *
 * Writes `<tag>.ndjson` (the samples, appended as they are drained) and `<tag>.page.json` (the
 * environment, console notes, and voids). Exits nonzero when the row could not be played at all.
 *
 * @module
 */
import { appendFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { parseArgs } from "node:util";
import type { Page } from "playwright";
import { launch, sleep } from "../../../interop/clients/js/harness.ts";
import type { Environment, Ring, Void } from "./src/schema.ts";

const { values } = parseArgs({
	options: {
		url: { type: "string" },
		fingerprint: { type: "string" },
		broadcast: { type: "string" },
		page: { type: "string" },
		ring: { type: "string" },
		delay: { type: "string", default: "auto" },
		duration: { type: "string", default: "60" },
		tag: { type: "string" },
		out: { type: "string" },
	},
});

const { url, fingerprint, broadcast, page: pageDir, tag, out } = values;
const durationMs = Number.parseFloat(values.duration) * 1000;
const ring = values.ring as Ring;
if (
	!url ||
	!fingerprint ||
	!broadcast ||
	!pageDir ||
	!tag ||
	!out ||
	!Number.isFinite(durationMs) ||
	durationMs <= 0 ||
	(ring !== "plain" && ring !== "isolated")
) {
	console.error(
		"usage: driver.ts --url U --fingerprint F --broadcast B --page DIR --ring plain|isolated --tag T --out DIR [--delay auto] [--duration S>0]",
	);
	process.exit(2);
}

/**
 * How long the session and the first audio get, together, before the row is abandoned. It stays short
 * of the step profile's change at 30s, so a step row always measures audio from before the change.
 */
const STARTUP_MS = 20_000;
const startupDeadline = Date.now() + STARTUP_MS;
/** How often the probe is drained into the ndjson. */
const DRAIN_MS = 1000;

const hash = await fetch(fingerprint).then((r) => {
	if (!r.ok) throw new Error(`${fingerprint}: ${r.status}`);
	return r.text();
});

// Cross-origin isolation is a property of the document, not the bundle, so one build served under
// two prefixes runs both rings. `/plain` is the production path: most viewers are not isolated, and
// get the postMessage ring.
const root = resolve(pageDir);
const isolation = { "cross-origin-opener-policy": "same-origin", "cross-origin-embedder-policy": "require-corp" };
const server = Bun.serve({
	port: 0,
	hostname: "127.0.0.1",
	async fetch(req) {
		const path = new URL(req.url).pathname;
		const match = path.match(/^\/(isolated|plain)\/(.*)$/);
		if (!match) return new Response("not found", { status: 404 });
		const file = Bun.file(join(root, match[2] || "index.html"));
		if (!(await file.exists())) return new Response("not found", { status: 404 });
		return new Response(file, { headers: match[1] === "isolated" ? isolation : undefined });
	},
});

const query = new URLSearchParams({ url, broadcast, delay: values.delay });
const pageUrl = `http://127.0.0.1:${server.port}/${ring}/?${query}`;
console.log(`page: ${pageUrl}`);

const samplesFile = join(out, `${tag}.ndjson`);
const voids: Void[] = [];
const refuse = (assertion: string, detail: string) => {
	console.error(`void: ${assertion}: ${detail}`);
	voids.push({ assertion, detail });
};

/** Poll the page until `ready` returns a value, or throw naming what never happened. */
async function waitFor<T>(page: Page, what: string, ready: () => T | undefined): Promise<T> {
	while (Date.now() < startupDeadline) {
		const value = await page.evaluate(ready);
		if (value !== undefined && value !== null) return value;
		await sleep(200);
	}
	throw new Error(`${what} not within ${STARTUP_MS / 1000}s`);
}

const drain = async (page: Page) => {
	const samples = await page.evaluate(() => globalThis.audioQuality.drain());
	if (samples.length > 0) appendFileSync(samplesFile, `${samples.map((s) => JSON.stringify(s)).join("\n")}\n`);
};

// No fake devices: nothing here captures. The autoplay flag stands in for the click a viewer makes.
const browser = await launch(["--autoplay-policy=no-user-gesture-required"]);
let environment: Environment | undefined;
let notes: string[] = [];
let failed = false;
try {
	const page = await browser.newPage();
	page.on("pageerror", (error) => console.error(`[page error] ${error.message}`));
	await page.route(new URL("/certificate.sha256", url).href, (route) =>
		route.fulfill({ body: hash, headers: { "access-control-allow-origin": "*" } }),
	);
	await page.goto(pageUrl, { waitUntil: "load" });

	environment = await waitFor(page, "session", () => {
		const env = globalThis.audioQuality?.environment();
		return env?.transport ? env : undefined;
	});
	if (environment.transport !== "webtransport") {
		refuse("transport", `negotiated ${environment.transport}, which never crosses the UDP shaper`);
	}
	if (environment.crossOriginIsolated !== (ring === "isolated")) {
		refuse("ring", `asked for ${ring}, but crossOriginIsolated is ${environment.crossOriginIsolated}`);
	}

	// The row's duration is audio played, not startup: the window starts once the ring does.
	await waitFor(page, "first audio", () => globalThis.audioQuality.playing() || undefined);

	const end = Date.now() + durationMs;
	while (Date.now() < end) {
		await sleep(Math.min(DRAIN_MS, end - Date.now()));
		await drain(page);
	}
	// A run that ends inside a gap still counts it.
	await page.evaluate(() => globalThis.audioQuality.finish());
	await drain(page);
	// The context rate is only known once the graph exists, which is after the catalog.
	environment = (await page.evaluate(() => globalThis.audioQuality.environment())) ?? environment;
	notes = await page.evaluate(() => globalThis.audioQuality.notes());
	await page.close();
} catch (err) {
	failed = true;
	refuse("driver", err instanceof Error ? err.message : String(err));
} finally {
	await browser.close().catch(() => {});
	server.stop(true);
}

await Bun.write(join(out, `${tag}.page.json`), JSON.stringify({ environment, notes, voids }, null, 1));
process.exit(failed ? 1 : 0);
