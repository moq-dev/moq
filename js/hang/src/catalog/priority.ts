// Defined in one place so the relative ordering stays consistent.
/** Delivery priority per track kind; higher is sent first. */
export const PRIORITY = {
	catalog: 100,
	audio: 80,
	video: 60,
} as const;
