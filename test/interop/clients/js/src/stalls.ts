/**
 * Logs every long animation frame the page renders, with the scripts that ran longest in it.
 *
 * A media window that fails because a page's main thread stopped for hundreds of milliseconds looks
 * the same from its samples whatever stopped it: a script of ours, the browser, or a starved host.
 * This log is what tells them apart. Importing the module starts it.
 *
 * @module
 */

import { LONG_FRAMES, type LongFrame } from "./contract";

/** Enough for minutes of a struggling page; the driver only ever reads the last few seconds. */
const KEEP = 500;

const log: LongFrame[] = [];
(globalThis as unknown as Record<string, LongFrame[]>)[LONG_FRAMES] = log;

type Script = { duration: number; invoker: string; sourceFunctionName: string; sourceURL: string };
type Entry = PerformanceEntry & { blockingDuration: number; scripts?: Script[] };

// Absent outside Chromium, where the log simply stays empty.
if (PerformanceObserver.supportedEntryTypes?.includes("long-animation-frame")) {
	new PerformanceObserver((list) => {
		for (const entry of list.getEntries() as Entry[]) {
			const scripts = [...(entry.scripts ?? [])]
				.sort((a, b) => b.duration - a.duration)
				.slice(0, 2)
				.map(
					(s) =>
						`${s.invoker} ${s.sourceFunctionName || "?"}@${s.sourceURL.split("/").pop() ?? ""} ${Math.round(s.duration)}ms`,
				);
			log.push({
				at: performance.timeOrigin + entry.startTime,
				duration: Math.round(entry.duration),
				blocking: Math.round(entry.blockingDuration),
				scripts: scripts.join(", ") || "no script",
			});
			if (log.length > KEEP) log.shift();
		}
	}).observe({ type: "long-animation-frame", buffered: true });
}
