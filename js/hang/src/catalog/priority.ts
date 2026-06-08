// Default priorities for the base catalog's tracks, kept together so the ordering is easy to
// eyeball. Applications pick their own priorities for sections they add (slotting around these).
export const PRIORITY = {
	catalog: 100,
	audio: 80,
	video: 60,
} as const;
