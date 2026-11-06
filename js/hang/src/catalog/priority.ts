// Defined in one place so the relative ordering stays consistent.
/**
 * Delivery priority per track kind, in `-128..=127` with higher sent first. All sit above 0, the
 * unset default, so hang media outranks a track that never set a priority.
 */
export const PRIORITY = {
	catalog: 100,
	text: 90,
	audio: 80,
	video: 60,
} as const;
