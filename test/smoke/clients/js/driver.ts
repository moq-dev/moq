/**
 * Drives a headless Chromium against the vite-built page (dist/) for the interop matrix. publish
 * streams fake camera/microphone input until killed; subscribe verifies rendered playback,
 * pause/resume, and optionally browser-to-browser audio.
 *
 * The publishers on the other side of the matrix are separate processes with no readiness signal,
 * so this driver tolerates a slow announcement with one reload. `media.ts` is the strict path: it
 * waits on an explicit publisher-ready state and never reloads.
 *
 *     bun driver.ts publish   --url http://127.0.0.1:4443 --broadcast b.hang
 *     bun driver.ts subscribe --url http://127.0.0.1:4443 --broadcast b.hang --timeout 20 [--expect-audio]
 *
 * A failing subscriber leaves a Playwright trace, a screenshot, and a HAR in
 * $MOQ_QA_TRACE, named by $MOQ_QA_LABEL. Both are set by the debug bundle (see
 * test/lib/bundle.sh); without them nothing is recorded.
 *
 * @module
 */
import { rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { parseArgs } from "node:util";
import { type Browser, type BrowserContext, type Page } from "playwright";
import {
	type BrowserErrors,
	launch,
	open,
	type PlayerState,
	POLL_INTERVAL_MS,
	pageUrl,
	readPlayerState,
	SELECTORS,
	serve,
	sleep,
	throwPageErrors,
	waitForState,
	waitForWatch,
} from "./harness";

const { positionals, values } = parseArgs({
	allowPositionals: true,
	options: {
		url: { type: "string" },
		broadcast: { type: "string" },
		timeout: { type: "string", default: "20" },
		"expect-audio": { type: "boolean", default: false },
	},
});

const role = positionals[0];
const url = values.url;
const broadcast = values.broadcast;
const timeoutMs = Number.parseFloat(values.timeout ?? "20") * 1000;
const expectAudio = values["expect-audio"] ?? false;
if (
	(role !== "publish" && role !== "subscribe") ||
	!url ||
	!broadcast ||
	!Number.isFinite(timeoutMs) ||
	timeoutMs <= 0 ||
	(expectAudio && role !== "subscribe")
) {
	console.error("usage: driver.ts publish|subscribe --url U --broadcast B [--timeout S>0] [--expect-audio]");
	process.exit(2);
}

const PAUSE_STABILITY_MS = 750;

async function waitForStablePause(page: Page, errors: BrowserErrors, deadline: number): Promise<PlayerState> {
	let previous = await readPlayerState(page);
	let stableSince = Date.now();
	while (Date.now() < deadline) {
		throwPageErrors(errors);
		await sleep(POLL_INTERVAL_MS);
		const current = await readPlayerState(page);
		const stable =
			current.videoFrames === previous.videoFrames &&
			current.videoTimestamp === previous.videoTimestamp &&
			(!expectAudio || current.audioBytes === previous.audioBytes);
		if (!stable) stableSince = Date.now();
		if (stable && Date.now() - stableSince >= PAUSE_STABILITY_MS) return current;
		previous = current;
	}
	throwPageErrors(errors);
	throw new Error(`playback did not stop after pause: ${JSON.stringify(previous)}`);
}

const server = serve();
let browser: Browser | undefined;
let context: BrowserContext | undefined;

// Only the subscriber. The publisher streams until the orchestrator SIGKILLs
// it, which is not an ending a trace or a HAR survives; its page errors reach
// the bundle through the log the orchestrator already captures.
const traceDir = role === "subscribe" ? process.env.MOQ_QA_TRACE : undefined;
const label = process.env.MOQ_QA_LABEL ?? `${role}-${process.pid}`;

// The HAR body content is omitted: the media never travels over HTTP anyway,
// and a bundle that ships payloads is one nobody can upload. WebTransport is
// invisible to both the trace and the HAR, which is what relay qlog is for.
let errors: BrowserErrors = { page: [], console: [] };

// A capture step that threw is the one case worth naming: a crashed Chromium
// rejects `tracing.stop` and the bundle then holds no trace, which is
// indistinguishable from a trace nobody asked for. Collected rather than
// discarded, and never fatal -- the failure under investigation is the run's,
// not the recorder's.
const captureFailures: string[] = [];
async function attempt(what: string, fn: () => Promise<unknown>): Promise<boolean> {
	try {
		await fn();
		return true;
	} catch (err) {
		captureFailures.push(`${what}: ${err instanceof Error ? err.message : String(err)}`);
		return false;
	}
}

// A trace is worth its megabytes only for a run that failed; a passing one
// discards everything it recorded. Returns the trace path when one was written.
async function saveTrace(page: Page | undefined, failed: boolean): Promise<string | undefined> {
	if (!context) {
		if (traceDir && failed) {
			captureFailures.push("context: browser context initialization did not complete");
			await writeFile(join(traceDir, `${label}.capture-failed.log`), `${captureFailures.join("\n")}\n`).catch(
				() => {},
			);
		}
		return undefined;
	}
	const activeContext = context;
	if (!traceDir) {
		await attempt("context.close", () => activeContext.close());
		return undefined;
	}
	const tracePath = join(traceDir, `${label}.trace.zip`);
	let wrote = false;
	if (failed) {
		wrote = await attempt("tracing.stop", () => activeContext.tracing.stop({ path: tracePath }));
		await attempt("screenshot", async () => {
			const capturePage = page ?? activeContext.pages()[0];
			if (!capturePage) throw new Error("the browser context has no page to capture");
			await capturePage.screenshot({ path: join(traceDir, `${label}.png`), fullPage: true });
		});
		const log = [...errors.page.map((e) => `page: ${e}`), ...errors.console.map((e) => `console: ${e}`)];
		await attempt("console log", () => writeFile(join(traceDir, `${label}.console.log`), `${log.join("\n")}\n`));
	} else {
		await attempt("tracing.stop", () => activeContext.tracing.stop());
	}
	// The HAR is only written on close, so it has to happen either way.
	await attempt("context.close", () => activeContext.close());
	if (!failed) await attempt("har cleanup", () => rm(join(traceDir, `${label}.har`), { force: true }));

	if (captureFailures.length > 0 && failed) {
		await writeFile(join(traceDir, `${label}.capture-failed.log`), `${captureFailures.join("\n")}\n`).catch(
			() => {},
		);
	}
	return wrote ? tracePath : undefined;
}

let code = 1;
let page: Page | undefined;
let failure: unknown;
let tracePath: string | undefined;
try {
	browser = await launch([
		"--use-fake-device-for-media-stream",
		"--use-fake-ui-for-media-stream",
		"--autoplay-policy=no-user-gesture-required",
	]);
	context = await browser.newContext(
		traceDir ? { recordHar: { path: join(traceDir, `${label}.har`), content: "omit" } } : {},
	);
	if (traceDir) await context.tracing.start({ screenshots: true, snapshots: true, sources: true });
	[page, errors] = await open(context, pageUrl(server.origin, role, { url, broadcast }));
	if (role === "subscribe") await waitForWatch(page);

	if (role === "publish") {
		console.error(`publishing ${broadcast} (fake camera + microphone) to ${url}`);
		await new Promise(() => {}); // stream until the orchestrator kills us
	} else {
		const start = Date.now();
		const startupDeadline = start + timeoutMs;
		let reloaded = false;
		let playing: PlayerState | undefined;
		while (Date.now() < startupDeadline) {
			throwPageErrors(errors);
			const state = await readPlayerState(page);
			if (state.videoTimestamp !== undefined && state.painted && state.controlLabel === "Pause") {
				playing = state;
				break;
			}
			// Retry once after the publisher has had time to announce. This preserves
			// the existing startup tolerance while the interaction checks below stay strict.
			if (!reloaded && Date.now() - start > timeoutMs / 2) {
				reloaded = true;
				await page.reload({ waitUntil: "load" });
				await waitForWatch(page);
			}
			await sleep(POLL_INTERVAL_MS);
		}
		if (!playing) {
			const state = await readPlayerState(page);
			throw new Error(`timed out waiting for rendered video: ${JSON.stringify(state)}`);
		}

		// Fault injection for the debug-bundle drills: fail mid-playback, so the
		// trace has a real session in it rather than an empty page.
		if (process.env.MOQ_QA_FAULT === "browser") {
			throw new Error("injected browser assertion failure (MOQ_QA_FAULT=browser)");
		}

		const interactionDeadline = Date.now() + timeoutMs;
		if (expectAudio) {
			playing = await waitForState(page, errors, {
				deadline: interactionDeadline,
				description: "browser audio",
				predicate: (state) => state.hasAudio && state.audioBytes > 0 && state.audioContext === "running",
			});
		}

		// The chrome auto-hides while playing. Pointer activity reveals the real
		// control, then the click must flow through the public player API.
		await page.dispatchEvent(SELECTORS.ui, "pointermove");
		await page.locator(SELECTORS.ui).locator(SELECTORS.pauseControl).click();
		await waitForState(page, errors, {
			deadline: interactionDeadline,
			description: "paused player UI",
			predicate: (state) =>
				state.paused && state.pausedAttribute && state.controlLabel === "Play" && state.centerPlayVisible,
		});
		const paused = await waitForStablePause(page, errors, interactionDeadline);
		if (!paused.painted) throw new Error(`pause cleared the preview frame: ${JSON.stringify(paused)}`);

		await page.locator(SELECTORS.ui).locator(SELECTORS.centerPlay).click();
		const resumed = await waitForState(page, errors, {
			deadline: interactionDeadline,
			description: "resumed playback",
			predicate: (state) =>
				!state.paused &&
				!state.pausedAttribute &&
				state.controlLabel === "Pause" &&
				!state.centerPlayVisible &&
				state.videoFrames > paused.videoFrames &&
				state.videoTimestamp !== undefined &&
				state.videoTimestamp > (paused.videoTimestamp ?? -1) &&
				(!expectAudio || state.audioBytes > paused.audioBytes),
		});

		throwPageErrors(errors);
		console.error(
			`rendered, paused, and resumed ${broadcast}: video=${resumed.videoFrames} frames` +
				(expectAudio ? ` audio=${resumed.audioBytes} bytes` : ""),
		);
		code = 0;
	}
} catch (err) {
	failure = err;
} finally {
	tracePath = await saveTrace(page, code !== 0);
	await browser?.close().catch(() => {});
	server.stop();
}

if (failure !== undefined) {
	console.error(failure instanceof Error ? (failure.stack ?? failure.message) : String(failure));
}
// Only claim a trace that exists. Pointing at one that was never written is
// worse than saying nothing, because it ends the search in the wrong place.
if (tracePath) console.error(`browser trace: ${tracePath}`);
for (const problem of captureFailures) console.error(`browser capture failed: ${problem}`);
process.exit(code);
