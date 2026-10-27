import { expect, test } from "bun:test";
import * as z from "@zod/mini";
import { Producer } from "./group.ts";
import { textFrame } from "./mock.ts";
import * as Zod from "./zod.ts";

test("read validates each frame and ends only at the end of the group", async () => {
	const producer = new Producer(0);
	const consumer = producer.consume();
	producer.writeFrame({ payload: new Uint8Array() });
	// Falsy values are frames like any other, not the end of the stream.
	for (const json of ["0", "false", "null", '"ok"']) producer.writeFrame(textFrame(json));
	producer.close();

	const schema = z.union([z.number(), z.boolean(), z.null(), z.string()]);
	await expect(Zod.read(consumer, schema)).rejects.toThrow(SyntaxError);
	expect(await Zod.read(consumer, schema)).toBe(0);
	expect(await Zod.read(consumer, schema)).toBe(false);
	expect(await Zod.read(consumer, schema)).toBe(null);
	expect(await Zod.read(consumer, schema)).toBe("ok");
	expect(await Zod.read(consumer, schema)).toBeUndefined();
});

test("read rejects a frame the schema refuses", async () => {
	const producer = new Producer(0);
	const consumer = producer.consume();
	producer.writeFrame(textFrame('{"count":"three"}'));
	producer.close();

	await expect(Zod.read(consumer, z.object({ count: z.number() }))).rejects.toThrow();
});
