import { now as clockNow, sleepUntil as clockSleepUntil, Instant, Micro, Nano, Second } from "@moq/clock";
import type { Timeline } from "./timeline";

// Not exported from `source/index.ts`: this is an implementation detail of the file source.

// A sample further behind the clock than this means nothing read the stream for a while, such as a
// capture parked while unwatched, rather than a briefly slow reader. Seeking to the present decodes
// at most a keyframe interval, while draining would decode and publish the whole gap.
export const SEEK_BEHIND = Micro.fromSecond(Second(1));

// The bits of a mediabunny sample the pacing loop touches. Video and audio samples only differ in
// what they convert to, which the caller supplies.
export interface Sample {
	readonly microsecondTimestamp: number;
	setTimestamp(seconds: number): void;
	close(): void;
}

export interface Sink<S extends Sample> {
	samples(startTimestamp?: number): AsyncGenerator<S, void, unknown>;
}

/**
 * Reads decoded samples out of a sink in real time, restarting the file when it ends.
 *
 * Decoding runs far faster than realtime, so holding each sample until its presentation time is
 * what makes this publish at 1x instead of blasting the whole file out at once. It also applies
 * backpressure: the sink only decodes as far as the reader pulls, and nothing ahead of that.
 *
 * A reader that returns after a long gap resumes at the present on the shared timeline, not at
 * the old cursor, so the file's other track stays in sync and the gap is never replayed.
 */
export function pace<S extends Sample, T>(
	sink: Sink<S>,
	timeline: Timeline,
	convert: (sample: S) => T,
	signal: AbortSignal,
): ReadableStream<T> {
	let samples = sink.samples();
	let loop = 0;

	return new ReadableStream<T>(
		{
			async pull(controller) {
				// Seek at most once per pull, so a decoder that can't keep up drains instead of reseeking forever.
				let sought = false;

				for (;;) {
					if (signal.aborted) {
						controller.close();
						return;
					}

					let next: IteratorResult<S, void>;
					try {
						next = await samples.next();
					} catch (err) {
						// Teardown disposes the Input, which makes any decode still in flight throw.
						if (!signal.aborted) throw err;
						controller.close();
						return;
					}

					if (next.done) {
						if (!timeline.loops) {
							controller.close();
							return;
						}

						// A fresh generator seeks back to the start; the offset keeps timestamps rising.
						loop++;
						samples = sink.samples();
						continue;
					}

					const sample = next.value;
					const timestamp = timeline.at(sample.microsecondTimestamp as Micro, loop);

					const present = now();
					if (!sought && present - timestamp > SEEK_BEHIND) {
						sample.close();
						sought = true;

						// The sink starts at the sample covering this position, decoding from its keyframe.
						const position = timeline.position(present);
						await samples.return();
						loop = position.loop;
						samples = sink.samples(Second.fromMicro(position.timestamp));
						continue;
					}

					// A sample that's slightly late goes out immediately, letting a slow reader catch up.
					await sleepUntil(timestamp, signal);
					if (signal.aborted) {
						sample.close();
						controller.close();
						return;
					}

					// Restamp onto our wall clock, the same epoch live capture uses, so a publish that
					// mixes sources keeps one timeline.
					sample.setTimestamp(Second.fromMicro(timestamp));
					controller.enqueue(convert(sample));
					sample.close();
					return;
				}
			},

			async cancel() {
				await samples.return();
			},
		},
		// Prefetching would park a stale sample in the queue while nothing reads, and hand it out on resume.
		{ highWaterMark: 0 },
	);
}

// The installed clock in microseconds, the epoch capture stamps frames against (see video/processor.ts).
export function now(): Micro {
	return Nano.toMicro(Nano(clockNow()));
}

// Wait until the given clock time, or until the effect tears down.
//
// A plain sleep rather than effect.timer because this runs once per sample: effect.timer registers a
// cleanup that only releases on teardown, which would pile up for as long as the file publishes.
export function sleepUntil(timestamp: Micro, signal: AbortSignal): Promise<void> {
	return clockSleepUntil(Instant(Micro.toNano(timestamp)), { signal }).catch(() => {});
}
