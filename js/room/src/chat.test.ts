import { expect, test } from "bun:test";
import { Manual, Nano } from "@moq/clock";
import { Broadcast, Time } from "@moq/net";
import { HISTORY, info, Publisher, Subscriber, TRACK } from "./chat.ts";

test("chat expires ten-second history and late readers only see retained messages", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const broadcast = new Broadcast.Producer();
	const publisher = Publisher.create(broadcast);
	const subscriber = Subscriber.subscribe(broadcast.consume());
	try {
		publisher.send("first");
		const first = await subscriber.recv();
		expect(first?.value).toEqual({ push: { index: 0, value: "first" } });
		// A sent message carries its send time.
		expect(first?.at).toBeDefined();
		await clock.advance(Nano(HISTORY - 1));
		publisher.expire();
		// The publisher's own timer retires the message once its history elapses.
		const popped = subscriber.recv();
		await clock.advance(Nano(1));
		expect((await popped)?.value).toEqual({ pop: { start: 0, end: 1 } });
		publisher.send("second");
		const late = Subscriber.subscribe(broadcast.consume());
		expect((await late.recv())?.value).toEqual({ push: { index: 1, value: "second" } });
		publisher.finish();
		expect(await late.recv()).toBeUndefined();
		late.close();
	} finally {
		publisher.finish();
		subscriber.close();
		broadcast.close();
		installed.close();
	}
});

test("chat rejects non-string window records and propagates track failures", async () => {
	const broadcast = new Broadcast.Producer();
	const track = broadcast.createTrack(TRACK, info());
	const subscriber = Subscriber.subscribe(broadcast.consume());
	try {
		track.writeFrame({
			payload: new TextEncoder().encode('{"offset":0,"records":[42]}'),
			timestamp: Time.Timestamp.now(),
		});
		await expect(subscriber.recv()).rejects.toThrow("chat record must be a string");
		track.close(new Error("chat aborted"));
		await expect(subscriber.recv()).rejects.toThrow("chat aborted");
	} finally {
		subscriber.close();
		broadcast.close();
	}
});
