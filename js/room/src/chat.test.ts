import { expect, spyOn, test } from "bun:test";
import { Broadcast } from "@moq/net";
import { HISTORY, Publisher, Subscriber, TRACK } from "./chat.ts";

test("chat expires ten-second history and late readers only see retained messages", async () => {
	const clock = spyOn(performance, "now").mockReturnValue(0);
	const broadcast = new Broadcast.Producer();
	const publisher = Publisher.create(broadcast);
	const subscriber = Subscriber.subscribe(broadcast.consume());
	try {
		publisher.send("first");
		expect(await subscriber.recv()).toEqual({ push: { index: 0, value: "first" } });
		clock.mockReturnValue(HISTORY - 1);
		publisher.expire();
		clock.mockReturnValue(HISTORY);
		publisher.expire();
		expect(await subscriber.recv()).toEqual({ pop: { start: 0, end: 1 } });
		publisher.send("second");
		const late = Subscriber.subscribe(broadcast.consume());
		expect(await late.recv()).toEqual({ push: { index: 1, value: "second" } });
		publisher.finish();
		expect(await late.recv()).toBeUndefined();
		late.close();
	} finally {
		publisher.finish();
		subscriber.close();
		broadcast.close();
		clock.mockRestore();
	}
});

test("chat rejects non-string window records and propagates track failures", async () => {
	const broadcast = new Broadcast.Producer();
	const track = broadcast.createTrack(TRACK);
	const subscriber = Subscriber.subscribe(broadcast.consume());
	try {
		track.writeString('{"offset":0,"records":[42]}');
		await expect(subscriber.recv()).rejects.toThrow("chat record must be a string");
		track.close(new Error("chat aborted"));
		await expect(subscriber.recv()).rejects.toThrow("chat aborted");
	} finally {
		subscriber.close();
		broadcast.close();
	}
});
