/**
 * Drives a headless Chromium against the vite-built page (see `serve`) for the interop matrix. publish
 * streams a fake camera and a microphone playing `--audio-file` until killed; subscribe verifies
 * rendered playback, that each audio channel the player outputs carries its `--tone`, and
 * pause/resume.
 *
 * The publishers on the other side of the matrix are separate processes with no readiness signal,
 * so this driver tolerates a slow announcement with one reload. `media.ts` is the strict path: it
 * waits on an explicit publisher-ready state and never reloads.
 *
 *     bun driver.ts publish   --url http://127.0.0.1:4443 --broadcast b.hang --audio-file tone.wav [--version V]
 *     bun driver.ts subscribe --url http://127.0.0.1:4443 --broadcast b.hang --timeout 20 --tone 440,660 [--version V]
 *
 * `--version` makes the element offer that protocol version alone.
 *
 * @module
 */
import { createReadStream } from "node:fs";
import { createInterface } from "node:readline/promises";
import { parseArgs } from "node:util";
import type { Page } from "playwright";
import {
	type BrowserErrors,
	Failure,
	finishTraces,
	launch,
	open,
	POLL_INTERVAL_MS,
	pageUrl,
	pause,
	readPlayerState,
	readResources,
	SELECTORS,
	serve,
	sleep,
	throwPageErrors,
	waitForState,
	waitForWatch,
} from "./harness";
import { type Sample, TONE_CHECK } from "./src/contract";

/** Remove the page's players, which closes their sessions, and wait until every transport has. */
async function closeSessions(page: Page): Promise<void> {
	await page
		.evaluate(() => {
			for (const el of document.querySelectorAll("moq-watch, moq-publish")) el.remove();
		})
		.catch(() => {});
	const deadline = Date.now() + 5000;
	while (Date.now() < deadline) {
		const live = await readResources(page).catch(() => undefined);
		if (!live || live.transports === 0) return;
		await sleep(POLL_INTERVAL_MS);
	}
	console.error("a session was still open 5 s after its player was removed");
}

const { positionals, values } = parseArgs({
	allowPositionals: true,
	options: {
		url: { type: "string" },
		broadcast: { type: "string" },
		version: { type: "string" },
		finish: { type: "boolean", default: false },
		"finish-ack": { type: "string" },
		frames: { type: "string", default: "10" },
		timeout: { type: "string", default: "20" },
		"audio-file": { type: "string" },
		tone: { type: "string" },
	},
});

const role = positionals[0];
const url = values.url;
const broadcast = values.broadcast;
const timeoutMs = Number.parseFloat(values.timeout ?? "20") * 1000;
const finishing = values.finish || values["finish-ack"] !== undefined;
const audioFile = values["audio-file"];
const tones = (values.tone ?? "").split(",").filter(Boolean).map(Number);
if (
	(role !== "publish" && role !== "subscribe") ||
	!url ||
	!broadcast ||
	!Number.isFinite(timeoutMs) ||
	timeoutMs <= 0 ||
	(!finishing && role === "publish" && !audioFile) ||
	(!finishing && role === "subscribe" && (!tones.length || tones.some((hz) => !(hz > 0))))
) {
	console.error(
		"usage: driver.ts publish --url U --broadcast B (--audio-file WAV | --finish-ack FIFO) | subscribe --url U --broadcast B (--tone HZ,... | --finish [--frames N]) [--version V] [--timeout S>0]",
	);
	process.exit(2);
}

/** Whether every channel the player outputs carries its own tone, judged as tone.py judges a native subscriber. */
function toneHeard(state: Sample): boolean {
	return (
		state.audioContext === "running" &&
		state.audioChannels === tones.length &&
		state.tones?.length === tones.length &&
		state.tones.every(({ rms, share }) => rms >= TONE_CHECK.minRms && share >= TONE_CHECK.minShare)
	);
}

const PAUSE_STABILITY_MS = 750;

/** How long audible audio may play before it has to carry the tone: the analyser's window, several times over. */
const TONE_GRACE_MS = 3000;

async function waitForStablePause(page: Page, errors: BrowserErrors, deadline: number): Promise<Sample> {
	let previous = await readPlayerState(page);
	let stableSince = Date.now();
	while (Date.now() < deadline) {
		throwPageErrors(errors);
		await sleep(POLL_INTERVAL_MS);
		const current = await readPlayerState(page);
		const stable =
			current.videoFrames === previous.videoFrames &&
			current.videoTimestamp === previous.videoTimestamp &&
			current.audioBytes === previous.audioBytes;
		if (!stable) stableSince = Date.now();
		if (stable && Date.now() - stableSince >= PAUSE_STABILITY_MS) return current;
		previous = current;
	}
	throwPageErrors(errors);
	throw new Error(`playback did not stop after pause: ${JSON.stringify(previous)}`);
}

const server = serve();
const browser = await launch([
	"--use-fake-device-for-media-stream",
	"--use-fake-ui-for-media-stream",
	"--autoplay-policy=no-user-gesture-required",
	// The fake microphone loops this file instead of its default beeps.
	...(audioFile ? [`--use-file-for-fake-audio-capture=${audioFile}`] : []),
]);

let code = 1;
try {
	const [page, errors] = await open(
		browser,
		pageUrl(server.origin, finishing ? `finish-${role}` : role, {
			url,
			broadcast,
			...(finishing ? { frames: values.frames ?? "10", timeout: String(timeoutMs) } : {}),
			...(values.version ? { versions: values.version } : {}),
			...(!finishing && role === "subscribe" ? { tone: tones.join(",") } : {}),
		}),
		role,
		role === "subscribe",
	);
	if (!finishing && role === "subscribe") await waitForWatch(page);

	if (finishing) {
		const wait = async (field: "ready" | "ended" | "result") => {
			const deadline = Date.now() + timeoutMs;
			while (Date.now() < deadline) {
				throwPageErrors(errors);
				const state = await page.evaluate(() => {
					const s = window.interopFinish;
					return (
						s && { ready: s.ready, ended: s.ended, result: s.result, error: s.error, lingerUs: s.lingerUs }
					);
				});
				if (state?.error) throw new Error(state.error);
				if (state?.[field]) return state;
				await sleep(POLL_INTERVAL_MS);
			}
			throw new Error(`finish ${field} timed out`);
		};
		const ready = await wait("ready");
		if (role === "subscribe") {
			console.error("finish reader ready");
			const input = createInterface({ input: process.stdin });
			try {
				if ((await input.question("")) !== "read") throw new Error("expected finish read release");
			} finally {
				input.close();
			}
			await page.evaluate(() => window.interopFinish?.read());
		} else {
			console.error(`finish ready linger_us=${ready.lingerUs}`);
			await wait("ended");
			console.error("finish ended");
			const input = createInterface({ input: createReadStream(values["finish-ack"] as string) });
			try {
				if ((await input[Symbol.asyncIterator]().next()).value !== "complete")
					throw new Error("expected finish acknowledgement");
			} finally {
				input.close();
			}
			await page.evaluate(() => window.interopFinish?.acknowledge());
		}
		console.error((await wait("result")).result);
		code = 0;
	} else if (role === "publish") {
		console.error(`publishing ${broadcast} (fake camera + ${audioFile} as the microphone) to ${url}`);
		// Stream until the orchestrator stops us.
		await new Promise((resolve) => process.once("SIGTERM", resolve));
		code = 0;
	} else {
		const start = Date.now();
		const startupDeadline = start + timeoutMs;
		let reloaded = false;
		let playing: Sample | undefined;
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

		const interactionDeadline = Date.now() + timeoutMs;
		try {
			// Once audio plays, the tone gets a bounded grace rather than the whole timeout, so a
			// wrong tone fails as fast as it does in the native subscribers.
			await waitForState(page, errors, {
				deadline: interactionDeadline,
				description: "audible audio",
				predicate: (state) => state.audioContext === "running" && (state.rms ?? 0) >= TONE_CHECK.minRms,
			});
			playing = await waitForState(page, errors, {
				deadline: Math.min(interactionDeadline, Date.now() + TONE_GRACE_MS),
				description: "the audio tone",
				predicate: toneHeard,
			});
		} catch (err) {
			// Only a tone wait that ran out is an audio check failure; a page error reports as itself.
			if (!(err instanceof Failure)) throw err;
			// `wrong tone` is the marker the harness's control requires: audible audio in the publisher's
			// layout that does not carry its tones. Anything else failing is a plain audio failure.
			const state = await readPlayerState(page);
			const wrongTone =
				state.audioContext === "running" &&
				state.audioChannels === tones.length &&
				state.tones?.length === tones.length &&
				state.tones.every(({ rms }) => rms >= TONE_CHECK.minRms);
			console.error(
				`audio check failed: ${wrongTone ? "wrong tone: " : ""}${state.audioChannels} catalog channels, ` +
					`per channel ${JSON.stringify(state.tones)}, want ${tones} Hz, loudest ${state.toneHz?.toFixed(0)} Hz`,
			);
			throw err;
		}

		await pause(page);
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
				state.audioBytes > paused.audioBytes,
		});

		throwPageErrors(errors);
		const heard = playing.tones?.map(({ share }, channel) => `${tones[channel]} Hz ${share.toFixed(2)}`).join(", ");
		console.error(
			`rendered, paused, and resumed ${broadcast}: video=${resumed.videoFrames} frames audio=${resumed.audioBytes} bytes (${heard})`,
		);
		code = 0;
	}
} finally {
	// `code` is 0 only when the role's checks all passed, so it decides whether the trace is kept.
	await finishTraces(code !== 0);
	// Close every session before the browser: closing it, or even the page, can end them without
	// a close, leaving the relay to time them out.
	for (const page of browser.contexts().flatMap((context) => context.pages())) {
		await page.evaluate(() => window.interopFinish?.close()).catch(() => {});
		await closeSessions(page);
		await page.close().catch(() => {});
	}
	await browser.close().catch(() => {});
	server.stop();
}
process.exit(code);
