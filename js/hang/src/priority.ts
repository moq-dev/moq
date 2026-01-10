// We define all of the priorities for tracks here.
// That way it's easier to make sure they are in the right order.
export const PRIORITY = {
	catalog: 100,
	chat: 90,
	audio: 80,
	video: 60,
	typing: 40,
	location: 20,
	preview: 10,
} as const;
