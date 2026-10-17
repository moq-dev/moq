/** Page-side observations every scenario shares. */

const tasks: PerformanceEntry[] = [];
const observer = new PerformanceObserver((list) => tasks.push(...list.getEntries()));
observer.observe({ type: "longtask", buffered: true });

/**
 * Long tasks overlapping `[start, end)`, on this page's clock.
 *
 * A task's entry only exists once it ends, so call this from a later task than the work.
 * Overlap, not start time: the task containing `start` began before it.
 */
export function longTasks(start: number, end: number): { count: number; total_ms: number } {
	tasks.push(...observer.takeRecords());
	const inside = tasks.filter((task) => task.startTime < end && task.startTime + task.duration > start);
	return { count: inside.length, total_ms: inside.reduce((sum, task) => sum + task.duration, 0) };
}

/** Resolves on a fresh task, so synchronous work before and after it lands in separate tasks. */
export function nextTask(): Promise<void> {
	return new Promise((resolve) => setTimeout(resolve, 0));
}

/** What this page runs on, so a comparison across a visibility or GPU change is refused. */
export function environment() {
	const gl = document.createElement("canvas").getContext("webgl");
	const info = gl?.getExtension("WEBGL_debug_renderer_info");
	return {
		visibility: document.visibilityState,
		user_agent: navigator.userAgent,
		hardware_concurrency: navigator.hardwareConcurrency,
		webgl_renderer: info && gl ? String(gl.getParameter(info.UNMASKED_RENDERER_WEBGL)) : "unavailable",
		device_pixel_ratio: devicePixelRatio,
	};
}

declare global {
	interface Window {
		environment: typeof environment;
	}
}
window.environment = environment;
