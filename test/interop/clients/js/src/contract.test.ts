import { expect, test } from "bun:test";
import * as Moq from "@moq/net";
import { lateJoinStartsLive, leakedPlayerStarted, readLiveGop, type Resources } from "./contract";

const playing: Resources = { transports: 1, sockets: 0, audioContexts: 1, workers: 0 };

test("a leaked player is visible when it reuses the pooled transport", () => {
	const leaked: Resources = { transports: 1, sockets: 0, audioContexts: 2, workers: 0 };
	expect(leakedPlayerStarted(playing, leaked)).toBe(true);
});

test("a leaked player is visible when it reuses a pooled websocket fallback", () => {
	const busy: Resources = { transports: 0, sockets: 1, audioContexts: 1, workers: 0 };
	const leaked: Resources = { transports: 0, sockets: 1, audioContexts: 2, workers: 0 };
	expect(leakedPlayerStarted(busy, leaked)).toBe(true);
});

test("unchanged counts are not a leak start", () => {
	expect(leakedPlayerStarted(playing, playing)).toBe(false);
});


test("late join uses the published GOP when painting is ahead of capture", async () => {
	const broadcast = new Moq.Broadcast.Producer();
	const track = broadcast.createTrack("video");
	try {
		const old = track.appendGroup();
		old.writeFrame({ payload: new Uint8Array([1]), timestamp: Moq.Time.Timestamp.fromMillis(3000) });
		old.close();
		const current = track.appendGroup();
		current.writeFrame({ payload: new Uint8Array([2]), timestamp: Moq.Time.Timestamp.fromMillis(3700) });
		current.writeFrame({ payload: new Uint8Array([3]), timestamp: Moq.Time.Timestamp.fromMillis(4199) });

		// Painted counter 127 can precede capture while GOP 111 remains current. The old
		// 127 - 111 <= 15 check rejects a valid join; an earlier published GOP must fail.
		const viewer = track.subscribe();
		const gop = await readLiveGop(broadcast.track("video"));
		expect(gop.timestamp).toBe(3700);
		expect(lateJoinStartsLive(gop, 3700)).toBe(true);
		expect(lateJoinStartsLive(gop, 4199)).toBe(true);
		expect(lateJoinStartsLive(gop, 3000)).toBe(false);
		expect(lateJoinStartsLive(gop, undefined)).toBe(false);
		expect(track.demand().used.peek()).toBe(true);
		const viewed = await viewer.recvGroup();
		expect((await viewed?.readFrame())?.payload).toEqual(new Uint8Array([2]));
		expect((await viewed?.readFrame())?.payload).toEqual(new Uint8Array([3]));
		viewed?.close();
		viewer.close();
		await track.demand().unused();
		expect(track.demand().used.peek()).toBe(false);
	} finally {
		broadcast.close();
	}
});
