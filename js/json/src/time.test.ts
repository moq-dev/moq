import { expect, test } from "bun:test";
import { Time, Track } from "@moq/net";
import { after } from "./time.ts";

test("after compares and bumps in the track's timescale", () => {
	const track = new Track.Producer("test").accept({ timescale: Time.Timescale.MICRO });
	const at = Time.Timestamp.fromMillis(5);
	expect(after(track, at)).toBe(at);

	track.appendGroup(at);
	// 5ms is 5000us, the newest start: a tie moves one microsecond past it, not one millisecond.
	const tie = after(track, at);
	expect(tie.scale).toBe(Time.Timescale.MICRO);
	expect(tie.value).toBe(5001);

	// A millisecond past 5000us passes it, though its own timescale is coarser than the track's.
	const later = Time.Timestamp.fromMillis(6);
	expect(after(track, later)).toBe(later);
});

test("after bumps a millisecond write that ties a finer start", () => {
	const track = new Track.Producer("test").accept({ timescale: Time.Timescale.MICRO });
	track.appendGroup(new Time.Timestamp(5_500, Time.Timescale.MICRO));
	// 5ms floors to 5000us, behind 5500us: in its own scale it would compare 5 against 5500.
	expect(after(track, Time.Timestamp.fromMillis(5)).value).toBe(5501);
});
