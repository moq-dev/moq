import { expect, test } from "bun:test";
import { Broadcast } from "@moq/net";
import { PRIORITY, Publisher, Subscriber, TRACK } from "./chat.ts";

async function pair(): Promise<{ publisher: Publisher; subscriber: Subscriber }> {
	const producer = new Broadcast.Producer();
	const publisher = Publisher.create(producer);
	const subscriber = Subscriber.subscribe(producer.consume());
	return { publisher, subscriber };
}

test("track name and priority", () => {
	expect(TRACK).toBe("chat");
	expect(PRIORITY).toBe(10);
});

test("roundtrip", async () => {
	const { publisher, subscriber } = await pair();

	publisher.send("hello");
	publisher.send("world");

	expect((await subscriber.recv())?.text).toBe("hello");
	expect((await subscriber.recv())?.text).toBe("world");
});

test("empty messages are skipped", async () => {
	const { publisher, subscriber } = await pair();

	publisher.send("");
	publisher.send("after empty");

	expect((await subscriber.recv())?.text).toBe("after empty");
});

test("closed track returns undefined", async () => {
	const { publisher, subscriber } = await pair();

	publisher.send("last");
	publisher.finish();

	expect((await subscriber.recv())?.text).toBe("last");
	expect(await subscriber.recv()).toBeUndefined();
});
