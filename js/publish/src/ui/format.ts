// Shared human-readable formatters for the publish UI.

/** Format a bitrate (bits per second) as Mbps/kbps/bps. */
export function formatBitrate(bps: number): string {
	if (bps >= 1_000_000) return `${(bps / 1_000_000).toFixed(1)} Mbps`;
	if (bps >= 1_000) return `${(bps / 1_000).toFixed(0)} kbps`;
	return `${Math.round(bps)} bps`;
}

/** Format an audio sample rate (Hz) as kHz. */
export function formatHz(hz: number): string {
	return `${(hz / 1000).toFixed(1)} kHz`;
}

/** Format a frame rate. */
export function formatFps(fps: number): string {
	return `${fps.toFixed(1)} fps`;
}
