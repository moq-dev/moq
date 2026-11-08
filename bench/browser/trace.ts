/** Chrome trace parsing shared by the browser benchmark runners. */

/** Categories a GC-attributing trace records. */
export const GC_CATEGORIES = [
	"devtools.timeline",
	"v8",
	"disabled-by-default-v8.gc",
	"disabled-by-default-devtools.timeline",
];

export interface TraceEvent {
	name: string;
	ph: string;
	pid: number;
	tid: number;
	dur?: number;
	args?: { data?: { frames?: unknown }; usedHeapSizeBefore?: number; usedHeapSizeAfter?: number };
}

export function traceEvents(buffer: Buffer): TraceEvent[] {
	const trace = JSON.parse(buffer.toString()) as { traceEvents?: TraceEvent[] } | TraceEvent[];
	return Array.isArray(trace) ? trace : (trace.traceEvents ?? []);
}

/** The renderer process showing `url`, from the frame list a trace starts with. */
export function rendererPid(events: TraceEvent[], url: string): number {
	for (const event of events) {
		if (event.name !== "TracingStartedInBrowser") continue;
		const frames = (event.args?.data?.frames ?? []) as { url: string; processId: number }[];
		const frame = frames.find((frame) => frame.url === url);
		if (frame) return frame.processId;
	}
	throw new Error(`no renderer process found for ${url}`);
}

/** GC on one renderer's main thread: total pause, count, and the JS heap each collection reclaimed. */
export function collections(events: TraceEvent[], pid: number) {
	const gcs = events.filter(
		(event) => event.pid === pid && event.tid === pid && (event.name === "MinorGC" || event.name === "MajorGC"),
	);
	const reclaimed = gcs.reduce(
		(sum, event) => sum + Math.max(0, (event.args?.usedHeapSizeBefore ?? 0) - (event.args?.usedHeapSizeAfter ?? 0)),
		0,
	);
	return {
		count: gcs.length,
		minor: gcs.filter((event) => event.name === "MinorGC").length,
		pause_ms: gcs.reduce((sum, event) => sum + (event.dur ?? 0), 0) / 1000,
		reclaimed_bytes: reclaimed,
	};
}
