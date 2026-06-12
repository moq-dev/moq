// Shared human-readable formatters for the watch UI.

/** Format a bitrate (bits per second) as Mbps/kbps/bps. */
export function formatBitrate(bps: number): string {
	if (bps >= 1_000_000) return `${(bps / 1_000_000).toFixed(1)} Mbps`;
	if (bps >= 1_000) return `${(bps / 1_000).toFixed(0)} kbps`;
	return `${Math.round(bps)} bps`;
}

/** Format a bandwidth estimate with a direction arrow, or null if unavailable. */
export function formatBandwidth(bps: number | undefined, dir: "up" | "down"): string | null {
	if (bps === undefined || bps <= 0) return null;
	const arrow = dir === "down" ? "↓" : "↑";
	if (bps >= 1_000_000_000) return `${arrow} ${(bps / 1_000_000_000).toFixed(1)} Gbps`;
	return `${arrow} ${formatBitrate(bps)}`;
}

/** Format a duration in milliseconds, switching to seconds past 1s. */
export function formatMillis(ms: number): string {
	if (ms < 1000) return `${Math.round(ms)} ms`;
	return `${(ms / 1000).toFixed(2)} s`;
}

/** Format an audio sample rate (Hz) as kHz. */
export function formatHz(hz: number): string {
	return `${(hz / 1000).toFixed(1)} kHz`;
}

/** Format a frame rate. */
export function formatFps(fps: number): string {
	return `${fps.toFixed(1)} fps`;
}
