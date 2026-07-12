import { expect, test } from "bun:test";
import * as Announce from "./announced.ts";
import * as Path from "./path.ts";

const p = (s: string) => Path.from(s);

test("next streams every appended event in order", async () => {
	const producer = new Announce.Producer();
	const consumer = producer.consume();

	producer.append({ path: p("a"), active: true });
	producer.append({ path: p("a"), active: false });

	expect(await consumer.next()).toEqual({ path: p("a"), active: true });
	expect(await consumer.next()).toEqual({ path: p("a"), active: false });
});

test("a same-name re-announce is a distinct update", async () => {
	const producer = new Announce.Producer();
	const consumer = producer.consume();

	// A republish (a lite-06 RESTART, or an unannounce+announce that coalesces) arrives as a
	// redundant active:true. The stream carries it as its own update, which is what lets a watcher
	// notice the new instance even though membership never observably flipped.
	producer.append({ path: p("a"), active: true });
	producer.append({ path: p("a"), active: true });

	expect(await consumer.next()).toEqual({ path: p("a"), active: true });
	expect(await consumer.next()).toEqual({ path: p("a"), active: true });
});

test("closing resolves next with undefined", async () => {
	const producer = new Announce.Producer();
	const consumer = producer.consume();

	producer.close();
	expect(await consumer.next()).toBeUndefined();
});

test("aborting rejects next", async () => {
	const producer = new Announce.Producer();
	const consumer = producer.consume();

	producer.close(new Error("boom"));
	await expect(consumer.next()).rejects.toThrow("boom");
});
