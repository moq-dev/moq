/** Page-side observations every scenario shares. */

const tasks: PerformanceEntry[] = [];
new PerformanceObserver((list) => tasks.push(...list.getEntries())).observe({ type: "longtask", buffered: true });

/** Long tasks that started inside `[start, end)`, on this page's clock. */
export function longTasks(start: number, end: number): { count: number; total_ms: number } {
	const inside = tasks.filter((task) => task.startTime >= start && task.startTime < end);
	return { count: inside.length, total_ms: inside.reduce((sum, task) => sum + task.duration, 0) };
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
