import { expect, mock, test } from "bun:test";
import { SessionCode } from "../error.ts";
import { ANNOUNCES, Interests, Slots, SUBSCRIPTIONS } from "./limits.ts";

test("the session defaults match moq-net", () => {
	expect(ANNOUNCES).toBe(100_000);
	expect(SUBSCRIPTIONS).toBe(10_000);
});

test("a full budget closes the session, and released slots can be used again", () => {
	const close = mock(() => {});
	const slots = new Slots(2, { close } as unknown as WebTransport);
	slots.acquire();
	slots.acquire();
	slots.release();
	slots.acquire();
	expect(() => slots.acquire()).toThrow("too many live requests");
	expect(close).toHaveBeenCalledWith({
		closeCode: SessionCode.TooManyRequests,
		reason: "remote error: 7 (too many live requests)",
	});
});

test.each(["track", "subscribe"] as const)("paired interests release the right charge when %s ends first", (first) => {
	const slots = new Slots(2);
	const interests = new Interests(slots);
	const track = interests.acquire("broadcast", "video", "track");
	const subscribe = interests.acquire("broadcast", "video", "subscribe");
	const other = interests.acquire("broadcast", "audio", "subscribe");
	expect(() => interests.acquire("broadcast", "video", "track")).toThrow();
	const held = { track, subscribe };
	held[first].release();
	expect(() => interests.acquire("other", "video", "subscribe")).toThrow();
	held[first === "track" ? "subscribe" : "track"].release();
	held[first].release();
	const replacement = interests.acquire("other", "video", "subscribe");
	expect(() => interests.acquire("third", "video", "track")).toThrow();
	other.release();
	replacement.release();
	interests.acquire("third", "video", "track").release();
});

test("duplicate interests count individually, and a failed acquisition leaves no charge", () => {
	const interests = new Interests(new Slots(2));
	const tracks = [interests.acquire("path", "track", "track"), interests.acquire("path", "track", "track")];
	const subscribes = [
		interests.acquire("path", "track", "subscribe"),
		interests.acquire("path", "track", "subscribe"),
	];
	expect(() => interests.acquire("path", "track", "subscribe")).toThrow();
	tracks[0]?.release();
	subscribes[0]?.release();
	const other = interests.acquire("path", "other", "track");
	expect(() => interests.acquire("path", "track", "subscribe")).toThrow();
	for (const held of [...tracks, ...subscribes, other]) held.release();
	interests.acquire("path", "track", "subscribe").release();
});
