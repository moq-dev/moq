/**
 * Subscriber-side measurement, taken at the sinks the user actually gets.
 *
 * Video is read back out of the `<canvas>` the renderer paints, so the number reported is the frame
 * a viewer is looking at, not a frame the decoder claims to have produced. Audio is read off an
 * analyser hung on the player's own graph root, so the tone reported is what feeds the speakers.
 * Both are sampled in the same tick, which is what makes their difference a synchronization
 * measurement rather than two unrelated timelines.
 *
 * Everything lands on the element's dataset. Playwright may evaluate in a different JavaScript
 * world, where DOM nodes are shared but custom-element instance fields are not, so a dataset string
 * is the only reading a driver can trust.
 *
 * @module
 */
import type MoqWatch from "@moq/watch/element";
import { type Resources, resources } from "./instrument";
import * as Pattern from "./pattern";

/** How often the page takes a sample. Fast enough to see a 200ms tone step, cheap enough to sustain. */
export const SAMPLE_MS = 50;

/** FFT window used to identify the tone. 2048 bins at 48kHz is ~23Hz wide and ~43ms long. */
const FFT_SIZE = 2048;

/** How far the tone peak must stand above the spectrum's median for the tone to count as present. */
export const TONE_FLOOR_DB = 15;

/** One measurement of both sinks. */
export type Sample = {
	/** Monotonic counter, so a driver can tell a fresh sample from a repeat of the last one. */
	seq: number;
	/** `performance.now()` when the sample was taken. */
	at: number;

	/** Whether the canvas holds anything but the renderer's black fill. */
	painted: boolean;
	/** The fixture frame counter read out of the canvas, absent when the picture is not the fixture. */
	frameId?: number;
	/** Gap between the fixture's reference blocks, 0-255. Absent when the picture is not the fixture. */
	contrast?: number;
	/** Frames the decoder has produced. */
	videoFrames: number;
	/** Presentation timestamp of the painted frame, in milliseconds. */
	videoTimestamp?: number;

	/** Whether the catalog offers audio at all. */
	hasAudio: boolean;
	/** Encoded audio bytes received. */
	audioBytes: number;
	/** `AudioContext.state`, absent until the graph exists. */
	audioContext?: string;
	/** Playback timestamp reported by the render worklet, in milliseconds. */
	audioTimestamp?: number;
	/** Whether the audio buffer is waiting to refill. */
	audioStalled: boolean;
	/** Peak frequency in the tone band, absent until the graph exists. */
	toneHz?: number;
	/** The tone step that peak names, absent when no tone stands above the floor. */
	toneStep?: number;
	/** Level of the tone peak, in dB. */
	toneDb?: number;
	/** Median level across the spectrum, in dB: the floor the tone has to beat. */
	noiseDb?: number;
	/** Root mean square of the graph root's output over the analyser window. */
	rms?: number;

	/** Whether the player is paused. */
	paused: boolean;
	/** Whether the `paused` attribute is reflected onto the element. */
	pausedAttribute: boolean;

	/** Platform resources the page still holds. See {@link Resources}. */
	resources: Resources;
};

// Median of a copy, used as the spectrum's noise floor. Median rather than mean so the tone itself
// (a handful of bins) does not lift the floor it is being compared against.
function median(values: Float32Array<ArrayBuffer>): number {
	const sorted = Float32Array.from(values).sort();
	return sorted[sorted.length >> 1];
}

// The strongest bin inside the tone table's band, plus the floor to judge it against.
function analyze(analyser: AnalyserNode, spectrum: Float32Array<ArrayBuffer>, wave: Float32Array<ArrayBuffer>) {
	analyser.getFloatFrequencyData(spectrum);
	analyser.getFloatTimeDomainData(wave);

	const binHz = analyser.context.sampleRate / analyser.fftSize;
	const first = Math.max(1, Math.floor((Pattern.STEP_BASE_HZ - Pattern.STEP_GAP_HZ) / binHz));
	const last = Math.min(spectrum.length - 1, Math.ceil(Pattern.stepFrequency(Pattern.STEPS) / binHz));

	let peak = first;
	for (let i = first; i <= last; i++) {
		if (spectrum[i] > spectrum[peak]) peak = i;
	}

	let energy = 0;
	for (const sample of wave) energy += sample * sample;

	const toneDb = spectrum[peak];
	const noiseDb = median(spectrum);
	const toneHz = peak * binHz;

	return {
		toneHz,
		toneDb,
		noiseDb,
		rms: Math.sqrt(energy / wave.length),
		toneStep: toneDb - noiseDb >= TONE_FLOOR_DB ? Pattern.nearestStep(toneHz) : undefined,
	};
}

// Read both the fixture counter and the "is anything painted at all" check off one pixel snapshot.
function readCanvas(canvas: HTMLCanvasElement | null) {
	if (!canvas || canvas.width === 0 || canvas.height === 0) return { painted: false };

	const pixels = canvas.getContext("2d")?.getImageData(0, 0, canvas.width, canvas.height).data;
	if (!pixels) return { painted: false };

	// Sample at most about 1,000 pixels and require actual color rather than the renderer's black fill.
	let painted = false;
	const stride = Math.max(4, Math.floor(pixels.length / 4000) * 4);
	for (let i = 0; i < pixels.length; i += stride) {
		if (pixels[i] + pixels[i + 1] + pixels[i + 2] > 12) {
			painted = true;
			break;
		}
	}

	return { painted, ...Pattern.decode(pixels, canvas.width, canvas.height) };
}

/**
 * Publish the page's live resource counts on `document.body`, forever.
 *
 * Separate from {@link attach} because the interesting reading is the one taken after the player is
 * gone, when there is no element left to hang state off.
 */
export function watchResources(): void {
	const sample = () => {
		document.body.dataset.smokeResources = JSON.stringify(resources());
	};
	sample();
	self.setInterval(sample, SAMPLE_MS);
}

/**
 * Start sampling a player onto `el.dataset.smokeState`, and mark it ready for a driver to poll.
 *
 * Returns a function that stops sampling.
 */
export function attach(el: MoqWatch): () => void {
	const spectrum = new Float32Array(FFT_SIZE / 2);
	const wave = new Float32Array(FFT_SIZE);
	let analyser: AnalyserNode | undefined;
	let root: AudioNode | undefined;
	let seq = 0;

	const sample = () => {
		// The graph root is rebuilt whenever the audio config changes, so re-hang the analyser rather
		// than measuring a node that no longer feeds anything.
		const current = el.audio.out.root.peek();
		if (current !== root) {
			root = current;
			analyser = undefined;
			if (current) {
				analyser = new AnalyserNode(current.context, { fftSize: FFT_SIZE, smoothingTimeConstant: 0 });
				current.connect(analyser);
			}
		}

		const state: Sample = {
			seq: seq++,
			at: performance.now(),
			...readCanvas(el.querySelector("canvas")),
			videoFrames: el.video.out.stats.peek()?.frameCount ?? 0,
			videoTimestamp: el.video.out.timestamp.peek() ?? undefined,
			hasAudio: el.catalog?.audio !== undefined,
			audioBytes: el.audio.out.stats.peek()?.bytesReceived ?? 0,
			audioContext: el.audio.out.context.peek()?.state,
			audioTimestamp: el.audio.out.timestamp.peek() ?? undefined,
			audioStalled: el.audio.out.stalled.peek(),
			...(analyser ? analyze(analyser, spectrum, wave) : {}),
			paused: el.paused,
			pausedAttribute: el.hasAttribute("paused"),
			resources: resources(),
		};

		el.dataset.smokeState = JSON.stringify(state);
	};

	sample();
	el.dataset.smokeReady = "";

	const timer = self.setInterval(sample, SAMPLE_MS);
	return () => self.clearInterval(timer);
}
