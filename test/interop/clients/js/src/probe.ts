/**
 * Subscriber-side measurement, taken at the sinks the user actually gets.
 *
 * Video is read back out of the `<canvas>` the renderer paints, so the number reported is the frame
 * a viewer is looking at, not a frame the decoder claims to have produced. Audio is read off an
 * analyser hung on the player's own graph root, so the tone reported is what feeds the speakers.
 * Both are sampled in the same tick, which is what makes their difference a synchronization
 * measurement rather than two unrelated timelines.
 *
 * Each sample is logged to the console for the driver. Reading the page instead would mean
 * evaluating in it, and Playwright evaluation grants user activation, which would invalidate the
 * gesture checks.
 *
 * @module
 */
import type MoqWatch from "@moq/watch/element";
import { AUDIBLE_RMS, SAMPLE_EVENT, SAMPLE_MS, type Sample, TONE_FLOOR_DB } from "./contract";
import { resources, speaker } from "./instrument";
import * as Pattern from "./pattern";

/** FFT window used to identify the tone. 2048 bins at 48kHz is ~23Hz wide and ~43ms long. */
const FFT_SIZE = 2048;

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
	const first = Math.max(1, Math.floor(Pattern.BAND.lowHz / binHz));
	const last = Math.min(spectrum.length - 1, Math.ceil(Pattern.BAND.highHz / binHz));

	let peak = first;
	for (let i = first; i <= last; i++) {
		if (spectrum[i] > spectrum[peak]) peak = i;
	}

	let energy = 0;
	for (const sample of wave) energy += sample * sample;

	const toneDb = spectrum[peak];
	const noiseDb = median(spectrum);
	const toneHz = peak * binHz;
	const rms = Math.sqrt(energy / wave.length);

	// Amplitude first, then shape. Digital silence puts most of the spectrum at -Infinity, which
	// makes any finite peak stand infinitely far "above the floor": the dB margin alone would call
	// silence a tone. The waveform cannot be argued with, so it decides whether there is audio at
	// all, and the margin only decides whether that audio is the fixture's tone.
	const audible = rms >= AUDIBLE_RMS && Number.isFinite(toneDb) && Number.isFinite(noiseDb);

	return {
		toneHz,
		toneDb,
		noiseDb,
		rms,
		toneStep: audible && toneDb - noiseDb >= TONE_FLOOR_DB ? Pattern.nearestStep(toneHz) : undefined,
	};
}

/**
 * Samples per channel the tone check judges: ~170ms at 48kHz. Long enough that a tone 9% off
 * frequency (a 44.1kHz stream played as 48kHz) has drifted well out of phase with the filter, and
 * short enough that a window falls between the playout hiccups two busy headless browsers cause.
 */
const TONE_WINDOW = 8192;

/** Fraction of the samples' energy at `hz`, by a Goertzel filter: 1.0 for a pure tone at that frequency. */
function goertzelShare(samples: Float32Array<ArrayBuffer>, rate: number, hz: number): number {
	const coeff = 2 * Math.cos((2 * Math.PI * hz) / rate);
	let s1 = 0;
	let s2 = 0;
	let energy = 0;
	for (const x of samples) {
		const s0 = x + coeff * s1 - s2;
		s2 = s1;
		s1 = s0;
		energy += x * x;
	}
	if (energy === 0) return 0;
	return (2 * (s1 * s1 + s2 * s2 - coeff * s1 * s2)) / (samples.length * energy);
}

/** One analyser per channel of the graph root, each judged against that channel's tone. */
class ToneCheck {
	#analysers: AnalyserNode[];
	#tones: number[];
	#wave = new Float32Array(TONE_WINDOW);

	constructor(root: AudioNode, tones: number[]) {
		this.#tones = tones;
		const splitter = new ChannelSplitterNode(root.context, { numberOfOutputs: tones.length });
		root.connect(splitter);
		this.#analysers = tones.map((_, channel) => {
			const analyser = new AnalyserNode(root.context, { fftSize: TONE_WINDOW });
			splitter.connect(analyser, channel);
			return analyser;
		});
	}

	read(): { rms: number; share: number }[] {
		return this.#analysers.map((analyser, channel) => {
			analyser.getFloatTimeDomainData(this.#wave);
			let energy = 0;
			for (const x of this.#wave) energy += x * x;
			return {
				rms: Math.sqrt(energy / this.#wave.length),
				share: goertzelShare(this.#wave, analyser.context.sampleRate, this.#tones[channel]),
			};
		});
	}
}

// The level reaching the speakers, after the player's volume and mute. Zero once nothing is connected.
function speakerRms(context: BaseAudioContext | undefined, wave: Float32Array<ArrayBuffer>): number | undefined {
	if (!context) return undefined;
	const analyser = speaker(context);
	if (!analyser) return 0;
	analyser.getFloatTimeDomainData(wave);
	let energy = 0;
	for (const sample of wave) energy += sample * sample;
	return Math.sqrt(energy / wave.length);
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

	return {
		painted,
		frameWidth: canvas.width,
		frameHeight: canvas.height,
		...Pattern.decode(pixels, canvas.width, canvas.height),
	};
}

/**
 * Publish the page's live resource counts on `document.body`, forever.
 *
 * Separate from {@link attach} because the interesting reading is the one taken after the player is
 * gone, when there is no element left to hang state off.
 */
export function watchResources(): void {
	const sample = () => {
		document.body.dataset.interopResources = JSON.stringify(resources());
	};
	sample();
	self.setInterval(sample, SAMPLE_MS);
}

/**
 * Start sampling a player to the console, beginning immediately.
 *
 * With `tones`, the frequency each channel should carry, every sample also judges each channel
 * against its tone.
 *
 * Returns a function that stops sampling.
 */
export function attach(el: MoqWatch, tones: number[] = []): () => void {
	const spectrum = new Float32Array(FFT_SIZE / 2);
	const wave = new Float32Array(FFT_SIZE);
	let analyser: AnalyserNode | undefined;
	let toneCheck: ToneCheck | undefined;
	let root: AudioNode | undefined;
	let seq = 0;

	const sample = () => {
		// The graph root is rebuilt whenever the audio config changes, so re-hang the analyser rather
		// than measuring a node that no longer feeds anything.
		const current = el.audio.out.root.peek();
		if (current !== root) {
			root = current;
			analyser = undefined;
			toneCheck = undefined;
			if (current) {
				analyser = new AnalyserNode(current.context, { fftSize: FFT_SIZE, smoothingTimeConstant: 0 });
				current.connect(analyser);
				if (tones.length) toneCheck = new ToneCheck(current, tones);
			}
		}
		const audioRendition = el.audio.source.out.config.peek();
		const audioName = el.audio.source.out.track.peek();

		const ui = document.querySelector("moq-watch-ui")?.shadowRoot;
		const control = ui?.querySelector<HTMLButtonElement>("button.control[aria-label]");
		const centerPlay = ui?.querySelector<HTMLButtonElement>("button.center-play");
		const state: Sample = {
			seq: seq++,
			broadcastActive: el.broadcast.out.active.peek() !== undefined,
			broadcastEpoch: el.broadcast.out.active.peek()?.epoch,
			broadcastStatus: el.broadcast.out.status.peek(),
			userActivated: navigator.userActivation.hasBeenActive,
			at: performance.now(),
			...readCanvas(el.querySelector("canvas")),
			videoFrames: el.video.out.stats.peek()?.frameCount ?? 0,
			videoTimestamp: el.video.out.timestamp.peek() ?? undefined,
			videoCodedWidth: el.video.out.frame.peek()?.codedWidth,
			videoCodedHeight: el.video.out.frame.peek()?.codedHeight,
			videoTrack: el.catalog?.video?.renditions.video?.track ?? "video",
			videoCodec: el.catalog?.video?.renditions.video?.codec,
			hasAudio: el.catalog?.audio !== undefined,
			audioBytes: el.audio.out.stats.peek()?.bytesReceived ?? 0,
			audioContext: el.audio.out.context.peek()?.state,
			audioTimestamp: el.audio.out.timestamp.peek() ?? undefined,
			audioStalled: el.audio.out.stalled.peek(),
			delay: el.sync.out.delay.peek(),
			...(analyser ? analyze(analyser, spectrum, wave) : {}),
			audioCodec: audioRendition?.codec,
			audioChannels: audioRendition?.numberOfChannels,
			audioSampleRate: audioRendition?.sampleRate,
			audioRendition: audioName,
			audioTrack: audioRendition?.track ?? audioName,
			speakerRms: speakerRms(el.audio.out.context.peek(), wave),
			tones: toneCheck?.read(),
			controlLabel: control?.getAttribute("aria-label") ?? undefined,
			centerPlayVisible: centerPlay ? getComputedStyle(centerPlay).display !== "none" : false,
			paused: el.paused,
			pausedAttribute: el.hasAttribute("paused"),
			resources: resources(),
		};

		console.debug(SAMPLE_EVENT + JSON.stringify(state));
	};

	sample();

	const timer = self.setInterval(sample, SAMPLE_MS);
	return () => self.clearInterval(timer);
}
