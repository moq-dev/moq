/**
 * Browser media QA: does a viewer actually get advancing, synchronized media, and does the player
 * recover from the lifecycle changes an application makes?
 *
 * The interop matrix (`driver.ts`) answers "did bytes arrive and did a pixel light up". That passes
 * on a frozen picture, on silence, and on audio a second out of step. This drives the deterministic
 * fixture instead (`src/fixture.ts`), whose media says what it is, and measures both sinks: the
 * frame counter painted on the canvas and the tone step on the audio graph. Everything asserted
 * here is browser output. Nothing here says anything about what a physical speaker emits.
 *
 * Launches with no Chromium flags at all - no fake camera, no fake permission prompt, no autoplay
 * override - and never reloads the page: the publisher reports when it is ready.
 *
 *     bun media.ts --url http://127.0.0.1:4443 [--timeout 30] [--cases pause,detach]
 *     bun media.ts --url ... --fault silent-audio --cases none --expect-fail "audio tone"
 *
 * @module
 */
import { parseArgs } from "node:util";
import type { Browser, Page } from "playwright";
import {
	type BrowserErrors,
	check,
	command,
	Failure,
	launch,
	open,
	type PlayerState,
	POLL_INTERVAL_MS,
	pageUrl,
	readFixtureState,
	readPlayerState,
	SELECTORS,
	serve,
	sleep,
	throwPageErrors,
	waitForFixture,
	waitForResources,
	waitForState,
	waitForWatch,
} from "./harness";
import { FAULTS, leakedPlayerStarted, SAMPLE_MS, SAMPLE_RATE } from "./src/contract";
import * as Pattern from "./src/pattern";

/** Cases beyond the mandatory capability probe, publisher readiness, and cold start. */
const CASES = ["pause", "rejoin", "detach", "republish", "late-join"] as const;
type Case = (typeof CASES)[number];

const { values } = parseArgs({
	options: {
		url: { type: "string" },
		timeout: { type: "string", default: "30" },
		fault: { type: "string", default: "none" },
		cases: { type: "string" },
		leak: { type: "boolean", default: false },
		"expect-fail": { type: "string" },
	},
});

const url = values.url;
const timeoutMs = Number.parseFloat(values.timeout ?? "30") * 1000;
const fault = values.fault ?? "none";
const expectFail = values["expect-fail"];
const selected = new Set<string>(
	values.cases === undefined ? CASES : values.cases === "none" ? [] : values.cases.split(","),
);
const unknown = [...selected].filter((name) => !CASES.some((c) => c === name));
if (!url || !Number.isFinite(timeoutMs) || timeoutMs <= 0 || !FAULTS.some((f) => f === fault) || unknown.length > 0) {
	console.error(
		`usage: media.ts --url U [--timeout S>0] [--fault ${FAULTS.join("|")}] [--cases none|${CASES.join(",")}] [--leak] [--expect-fail TEXT]`,
	);
	process.exit(2);
}
const wants = (name: Case) => selected.has(name);

// process.exit narrows `url` above, but not inside the function declarations below.
const relay: string = url;

/** How long each media measurement window runs. Long enough to cover several tone cycles. */
const WINDOW_MS = 4000;

/** How long a state change (pause, unsubscribe, republish) has to take effect. */
const SETTLE_MS = 6000;

/** Fraction of a window's samples that must be readable, carry a tone, and be in sync. */
const AGREEMENT = 0.9;

/**
 * Presented frames per second a window must sustain, as a fraction of the fixture's rate.
 *
 * Well under 1: the probe samples asynchronously, the encoder drops frames under load, and a
 * headless run shares a CPU with the publisher. The bar is "the picture is advancing at roughly
 * the source rate", not "every frame arrived".
 */
const MIN_RATE = 0.5;

/**
 * Tolerated audio/video skew, in tone steps, so 200ms per step either way.
 *
 * The analyser's window straddles a step boundary for about a fifth of every step, and the canvas
 * holds the frame painted up to one frame ago, so a perfectly synchronized stream still reads one
 * step off some of the time. Two steps is not boundary noise.
 */
const MAX_SKEW_STEPS = 1;

const percentile = (values: number[], p: number) => {
	if (values.length === 0) return Number.NaN;
	const sorted = [...values].sort((a, b) => a - b);
	return sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * p))];
};

/** Collect distinct samples from the player for `ms`, so a window can be measured after the fact. */
async function collect(page: Page, errors: BrowserErrors, ms: number): Promise<PlayerState[]> {
	const samples: PlayerState[] = [];
	const deadline = Date.now() + ms;
	while (Date.now() < deadline) {
		throwPageErrors(errors);
		const sample = await readPlayerState(page).catch(() => undefined);
		if (sample && sample.seq !== samples[samples.length - 1]?.seq) samples.push(sample);
		await sleep(SAMPLE_MS / 2);
	}
	throwPageErrors(errors);
	return samples;
}

/** How long the presented frame must hold still to count as stopped. */
const FROZEN_MS = 1000;

/** How long the picture is watched after a pause settles, to prove it stays on the one frame. */
const HELD_MS = 1500;

/**
 * Wait until the presented frame stops moving, and return the frame it stopped on.
 *
 * The picture, not a status flag, is what a viewer sees stop, so this is what "playback stopped"
 * has to mean. A canvas that goes unreadable counts as stopped too.
 */
async function waitFrozen(
	page: Page,
	errors: BrowserErrors,
	assertion: string,
	description: string,
): Promise<number> {
	const deadline = Date.now() + SETTLE_MS;
	let frame: number | undefined;
	let since = Date.now();
	while (Date.now() < deadline) {
		throwPageErrors(errors);
		const current = (await readPlayerState(page).catch(() => undefined))?.frameId;
		if (current !== frame) {
			frame = current;
			since = Date.now();
		} else if (Date.now() - since >= FROZEN_MS) {
			return frame ?? 0;
		}
		await sleep(POLL_INTERVAL_MS);
	}
	throwPageErrors(errors);
	throw new Failure(assertion, `${description}: the presented frame is still advancing, now ${frame}`);
}

/** How many samples of the failing window to print, so a summary can be traced back to readings. */
const TRACE_SAMPLES = 40;

// One sample as a line: elapsed time, the frame on the canvas, the tone step heard against the one
// that frame belongs to, and how far the tone stood above the noise floor.
function traceLine(sample: PlayerState, start: number): string {
	const step = sample.frameId === undefined ? "?" : Pattern.expectedStep(sample.frameId);
	const margin = sample.toneDb !== undefined && sample.noiseDb !== undefined ? sample.toneDb - sample.noiseDb : 0;
	return (
		`    +${((sample.at - start) / 1000).toFixed(2)}s frame=${sample.frameId ?? "-"} ` +
		`step=${sample.toneStep ?? "-"}/${step} tone=${margin.toFixed(0)}dB ${sample.toneHz?.toFixed(0) ?? "-"}Hz ` +
		`paused=${sample.paused}`
	);
}

/**
 * Assert the window shows advancing, audible, synchronized media, and report what it measured.
 *
 * Every number here is read at a sink: the frame counter off the canvas the renderer paints, and
 * the tone off the graph root that feeds the speakers. A failure prints the tail of the window it
 * measured, so the summary can be traced back to the readings behind it.
 */
function assertMedia(samples: PlayerState[], label: string): void {
	try {
		measure(samples, label);
	} catch (err) {
		const start = samples[0]?.at ?? 0;
		console.error(`  ${label}: last ${Math.min(TRACE_SAMPLES, samples.length)} of ${samples.length} samples`);
		for (const sample of samples.slice(-TRACE_SAMPLES)) console.error(traceLine(sample, start));
		throw err;
	}
}

function measure(samples: PlayerState[], label: string): void {
	check(samples.length >= 10, "sampling", () => `${label}: only ${samples.length} samples in the window`);

	const readable = samples.filter((s) => s.frameId !== undefined);
	check(
		readable.length >= samples.length * AGREEMENT,
		"pattern readable",
		() => `${label}: ${readable.length}/${samples.length} presented frames carried a readable fixture pattern`,
	);

	const first = readable[0];
	const last = readable[readable.length - 1];
	const elapsed = (last.at - first.at) / 1000;
	const advance = (last.frameId ?? 0) - (first.frameId ?? 0);
	const rate = advance / elapsed;
	const want = Pattern.FPS * MIN_RATE;
	check(
		rate >= want,
		"video progress",
		() =>
			`${label}: presented ${advance} frames in ${elapsed.toFixed(1)}s, ${rate.toFixed(1)}fps against a ${want.toFixed(1)}fps floor`,
	);

	const back = readable.findIndex((s, i) => i > 0 && (s.frameId ?? 0) < (readable[i - 1].frameId ?? 0));
	check(
		back < 0,
		"video monotonic",
		() =>
			`${label}: presented frame went backwards, ${readable[back - 1]?.frameId} then ${readable[back]?.frameId}`,
	);

	const toned = samples.filter((s) => s.toneStep !== undefined);
	check(
		toned.length >= samples.length * AGREEMENT,
		"audio tone",
		() => `${label}: ${toned.length}/${samples.length} samples carried the fixture tone above the noise floor`,
	);

	const skews = samples
		.filter((s) => s.frameId !== undefined && s.toneStep !== undefined)
		.map((s) => Pattern.stepSkew(s.toneStep as number, Pattern.expectedStep(s.frameId as number)));
	check(skews.length > 0, "audio/video sync", () => `${label}: no sample carried both a frame and a tone`);

	const aligned = skews.filter((skew) => Math.abs(skew) <= MAX_SKEW_STEPS);
	const median = percentile(skews.map(Math.abs), 0.5) * Pattern.STEP_MS;
	const p95 = percentile(
		skews.map((s) => Math.abs(s) * Pattern.STEP_MS),
		0.95,
	);
	check(
		aligned.length >= skews.length * AGREEMENT && median <= MAX_SKEW_STEPS * Pattern.STEP_MS,
		"audio/video sync",
		() =>
			`${label}: ${aligned.length}/${skews.length} samples within ${MAX_SKEW_STEPS * Pattern.STEP_MS}ms, median skew ${median.toFixed(0)}ms`,
	);

	const margin = percentile(
		toned.map((s) => (s.toneDb ?? 0) - (s.noiseDb ?? 0)),
		0.5,
	);
	console.error(
		`  ${label}: ${rate.toFixed(1)}fps presented over ${advance} frames, tone ${margin.toFixed(0)}dB above the floor, ` +
			`skew median ${median.toFixed(0)}ms p95 ${p95.toFixed(0)}ms (browser output, not a speaker)`,
	);
}

/** Probe every platform API the player needs, rather than assuming this engine has them. */
async function capabilities(page: Page): Promise<void> {
	// The fixture's own configuration, so the probe answers "can this browser run this fixture"
	// rather than "can it run something like it".
	const wanted = { sampleRate: SAMPLE_RATE, width: Pattern.WIDTH, height: Pattern.HEIGHT };
	const found = await page.evaluate(async (wanted) => {
		const has = (name: string) => typeof (globalThis as Record<string, unknown>)[name] === "function";
		const probe = async (fn: () => Promise<{ supported?: boolean }>) => {
			try {
				return (await fn()).supported === true;
			} catch {
				return false;
			}
		};
		return {
			WebTransport: has("WebTransport"),
			VideoDecoder: has("VideoDecoder"),
			VideoEncoder: has("VideoEncoder"),
			AudioDecoder: has("AudioDecoder"),
			AudioEncoder: has("AudioEncoder"),
			AudioWorkletNode: has("AudioWorkletNode"),
			MediaStreamTrackProcessor: has("MediaStreamTrackProcessor"),
			"canvas.captureStream": typeof HTMLCanvasElement.prototype.captureStream === "function",
			"encode avc1.42001f": await probe(() =>
				VideoEncoder.isConfigSupported({ codec: "avc1.42001f", width: wanted.width, height: wanted.height }),
			),
			"encode opus": await probe(() =>
				AudioEncoder.isConfigSupported({ codec: "opus", sampleRate: wanted.sampleRate, numberOfChannels: 1 }),
			),
			"decode opus": await probe(() =>
				AudioDecoder.isConfigSupported({ codec: "opus", sampleRate: wanted.sampleRate, numberOfChannels: 1 }),
			),
		};
	}, wanted);

	for (const [name, ok] of Object.entries(found)) console.error(`  ${ok ? "yes" : "NO "}  ${name}`);

	const missing = Object.entries(found)
		.filter(([, ok]) => !ok)
		.map(([name]) => name);
	const version = page.context().browser()?.version() ?? "unknown";
	check(
		missing.length === 0,
		"capabilities",
		() => `chromium ${version} lacks ${missing.join(", ")}; these checks cannot run against it`,
	);
}

const server = serve();
const browsers: Browser[] = [];

// One browser per role, launched with no arguments at all: no fake camera, no fake permission
// prompt, no autoplay override. Pages in one browser share a renderer scheduler, and the one that
// is not frontmost is throttled and reported hidden, which stalls both the fixture's clock and the
// player's download policy.
async function browserFor(): Promise<Browser> {
	const browser = await launch();
	browsers.push(browser);
	return browser;
}

/** Open a subscriber page and wait for the player to start sampling. Never reloads. */
async function subscriber(broadcast: string, label: string): Promise<[Page, BrowserErrors]> {
	const [page, errors] = await open(
		await browserFor(),
		// visible="always" because the window is never frontmost in a headless run, and the default
		// policy would stop downloading video and leave the canvas black.
		pageUrl(server.origin, "subscribe", { url: relay, broadcast, visible: "always" }),
		label,
	);
	await waitForWatch(page);
	return [page, errors];
}

// A real click, so the page carries user activation. Every audio path is gated on it.
const gesture = (page: Page) => page.mouse.click(1, 1);

let failure: Error | undefined;
try {
	const broadcast = `smoke-media-${process.pid}.hang`;

	// ── publisher ────────────────────────────────────────────────────────────
	const [publisher, publisherErrors] = await open(
		await browserFor(),
		pageUrl(server.origin, "fixture", { url: relay, broadcast, fault }),
		"fixture",
	);

	console.error("=== capabilities ===");
	await capabilities(publisher);

	console.error("=== publisher readiness ===");
	// What this run can and cannot say about the gesture gate. Chromium enforces it on the fixture
	// page (its audio graph stays suspended, and `resume()` never settles, until the click below)
	// but not consistently on the player's, whose graph is built later and has been seen running
	// with no activation at all. So the gate is exercised, not asserted: both pages are clicked and
	// both must carry audio afterwards. Asserting silence beforehand would measure this browser.
	console.error("  no fake-device or autoplay flags; each page is clicked before audio is required");

	await gesture(publisher);
	const ready = await waitForFixture(publisher, publisherErrors, {
		deadline: Date.now() + timeoutMs,
		assertion: "publisher readiness",
		description: "the fixture to announce a video and audio catalog and start its clock",
		predicate: (state) => state.ready && state.frameId > 0,
	});
	console.error(`  announced ${broadcast}, painting from frame ${ready.frameId}`);

	// ── cold start ───────────────────────────────────────────────────────────
	// No reload anywhere below. The publisher is known ready, so a subscriber that needs a second
	// page load to find the broadcast is an initialization bug, not a race.
	console.error("=== cold start ===");
	let [player, playerErrors] = await subscriber(broadcast, "player");

	// Video has to reach the canvas with no gesture at all: only audio is ever gated.
	const first = await waitForState(player, playerErrors, {
		deadline: Date.now() + timeoutMs,
		assertion: "cold start presents video",
		description: "the first presented fixture frame",
		predicate: (state) => state.frameId !== undefined,
	});
	console.error(`  presented frame ${first.frameId} before any gesture, audio ${first.audioContext ?? "absent"}`);

	await gesture(player);
	// Deliberately does not wait for a tone: whether audio actually carries the fixture is what
	// assertMedia measures, so silence has to fail there rather than time out here.
	await waitForState(player, playerErrors, {
		deadline: Date.now() + timeoutMs,
		assertion: "gesture starts audio",
		description: "the audio graph to resume after a user gesture",
		predicate: (state) => state.audioContext === "running",
	});

	assertMedia(await collect(player, playerErrors, WINDOW_MS), "cold start");

	// ── pause and resume ─────────────────────────────────────────────────────
	if (wants("pause")) {
		console.error("=== pause and resume ===");
		// The chrome auto-hides while playing; pointer activity reveals the real control.
		await player.dispatchEvent(SELECTORS.ui, "pointermove");
		await player.locator(SELECTORS.ui).locator(SELECTORS.pauseControl).click();
		await waitForState(player, playerErrors, {
			deadline: Date.now() + SETTLE_MS,
			assertion: "pause takes effect",
			description: "the player to report itself paused",
			predicate: (state) => state.paused && state.controlLabel === "Play",
		});

		// The frame the pause settled on, not the one showing when the flag flipped: a frame already
		// scheduled for presentation still lands, and that is one frame, not playback continuing.
		const paused = await waitFrozen(
			player,
			playerErrors,
			"pause holds the picture",
			"the presented frame never settled after pausing",
		);

		const held = await collect(player, playerErrors, HELD_MS);
		const moved = held.filter((s) => s.frameId !== undefined && s.frameId !== paused);
		check(
			moved.length === 0,
			"pause holds the picture",
			() => `presented frame moved from ${paused} to ${moved[moved.length - 1]?.frameId} while paused`,
		);
		check(
			held[held.length - 1]?.painted === true,
			"pause holds the picture",
			() => `pause cleared the preview frame: ${JSON.stringify(held[held.length - 1])}`,
		);

		await player.locator(SELECTORS.ui).locator(SELECTORS.centerPlay).click();
		const resumed = await waitForState(player, playerErrors, {
			deadline: Date.now() + SETTLE_MS,
			assertion: "resume takes effect",
			description: `the presented frame to move past the ${paused} the pause held`,
			predicate: (state) => !state.paused && (state.frameId ?? 0) > paused,
		});
		console.error(`  resumed ${(resumed.frameId ?? 0) - paused} frames past the pause`);
		assertMedia(await collect(player, playerErrors, WINDOW_MS), "after resume");
	}

	// ── unsubscribe and rejoin ───────────────────────────────────────────────
	if (wants("rejoin")) {
		console.error("=== unsubscribe and rejoin ===");
		await player.locator(SELECTORS.watch).evaluate((el) => el.setAttribute("name", "smoke-media-nowhere.hang"));
		const left = await waitFrozen(
			player,
			playerErrors,
			"unsubscribe stops playback",
			"the player kept presenting a broadcast it no longer subscribes to",
		);

		await player.locator(SELECTORS.watch).evaluate((el, name) => el.setAttribute("name", name), broadcast);
		await waitForState(player, playerErrors, {
			deadline: Date.now() + timeoutMs,
			assertion: "rejoin resumes playback",
			description: `the presented frame to move past the ${left} it stopped on`,
			predicate: (state) => (state.frameId ?? 0) > left,
		});
		assertMedia(await collect(player, playerErrors, WINDOW_MS), "after rejoin");
	}

	// ── detach and reattach ──────────────────────────────────────────────────
	if (wants("detach")) {
		console.error("=== detach and reattach ===");
		const busy = await readPlayerState(player);
		// Its own assertion name, not "resource baseline": the leaked-session control expects that one
		// to fail, so sharing it would let broken instrumentation satisfy the control without the
		// leaking detach ever running.
		check(
			busy.resources.transports + busy.resources.sockets > 0 && busy.resources.audioContexts > 0,
			"resource instrumentation",
			() => `the player holds nothing to release while playing: ${JSON.stringify(busy.resources)}`,
		);

		if (values.leak) {
			await command(player, "startLeak");
			await waitForResources(player, playerErrors, {
				deadline: Date.now() + SETTLE_MS,
				assertion: "resource instrumentation",
				description: `the deliberately leaked player to open another audio graph beyond ${JSON.stringify(busy.resources)}`,
				predicate: (r) => leakedPlayerStarted(busy.resources, r),
			});
		}
		await command(player, "detach");
		await waitForResources(player, playerErrors, {
			deadline: Date.now() + SETTLE_MS,
			assertion: "resource baseline",
			description: `every session, audio graph, and worker to be released (held ${JSON.stringify(busy.resources)} while playing)`,
			predicate: (r) => r.transports === 0 && r.sockets === 0 && r.audioContexts === 0 && r.workers === 0,
		});
		console.error("  detach released every session, audio graph, and worker");

		await command(player, "reattach");
		await waitForState(player, playerErrors, {
			deadline: Date.now() + timeoutMs,
			assertion: "reattach resumes playback",
			description: `the presented frame to move past the ${busy.frameId} showing before the detach`,
			predicate: (state) => (state.frameId ?? 0) > (busy.frameId ?? 0),
		});
		assertMedia(await collect(player, playerErrors, WINDOW_MS), "after reattach");
	}

	// ── publisher stop and same-path republish ───────────────────────────────
	if (wants("republish")) {
		console.error("=== stop and republish ===");
		const before = await readPlayerState(player);
		await command(publisher, "stop");
		await waitFrozen(
			player,
			playerErrors,
			"playback stops with the publisher",
			"the player kept presenting new frames after the publisher went away",
		);

		await command(publisher, "start");
		// The restarted fixture paints from zero again, so a recovered player is one presenting a
		// frame from before the stop: proof it followed the new announcement rather than a cache.
		const recovered = await waitForState(player, playerErrors, {
			deadline: Date.now() + timeoutMs,
			assertion: "republish serves the new stream",
			description: `a presented frame below the ${before.frameId} reached before the publisher stopped`,
			predicate: (state) => state.frameId !== undefined && state.frameId < (before.frameId ?? 0),
		});
		console.error(`  recovered at frame ${recovered.frameId}, restarted from ${before.frameId}`);
		assertMedia(await collect(player, playerErrors, WINDOW_MS), "after republish");
	}

	// ── late join ────────────────────────────────────────────────────────────
	// A fresh page against a broadcast that has been running for a while: the player has to tune in
	// at the live edge rather than replay what it missed.
	if (wants("late-join")) {
		console.error("=== late join ===");
		await player.close();
		const live = await readFixtureState(publisher);
		[player, playerErrors] = await subscriber(broadcast, "latecomer");
		await gesture(player);
		const joined = await waitForState(player, playerErrors, {
			deadline: Date.now() + timeoutMs,
			assertion: "late join presents media",
			description: "the latecomer to present the fixture",
			predicate: (state) => state.frameId !== undefined && state.audioContext === "running",
		});
		check(
			(joined.frameId ?? 0) >= live.frameId,
			"late join starts live",
			() => `joined at frame ${joined.frameId}, behind the ${live.frameId} already published when it opened`,
		);
		console.error(`  joined at frame ${joined.frameId}, live edge was ${live.frameId}`);
		assertMedia(await collect(player, playerErrors, WINDOW_MS), "late join");
	}
} catch (err) {
	failure = err instanceof Error ? err : new Error(String(err));
} finally {
	for (const browser of browsers) await browser.close().catch(() => {});
	server.stop();
}

if (expectFail !== undefined) {
	// A negative control. The run has to fail, and fail on the assertion it was aimed at: a pass, or
	// a failure somewhere else, both mean the assertion does not measure what it claims to.
	if (!failure) {
		console.error(`negative control passed, but it must fail on "${expectFail}"`);
		process.exit(1);
	}
	if (!(failure instanceof Failure) || failure.assertion !== expectFail) {
		console.error(`negative control was aimed at "${expectFail}" but broke elsewhere: ${failure.message}`);
		process.exit(1);
	}
	console.error(`negative control failed as required: ${failure.message}`);
	process.exit(0);
}

if (failure) {
	console.error(`FAIL ${failure.message}`);
	process.exit(1);
}
console.error("media: all checks passed");
process.exit(0);
