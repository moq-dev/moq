import { expect, test } from "bun:test";
import { Time, Track } from "@moq/net";
import { next } from "./time.ts";

test("next compares and bumps in the track's timescale", () => {
	const track = new Track.Producer("test").accept({ timescale: Time.Timescale.MICRO });
	const at = Time.Timestamp.fromMillis(5);
	expect(next(track, at)).toBe(at);

	track.appendGroup({ payload: new Uint8Array(), timestamp: at });
	// 5ms is 5000us, the newest start: a tie moves one microsecond past it, not one millisecond.
	const tie = next(track, at);
	expect(tie.scale).toBe(Time.Timescale.MICRO);
	expect(tie.value).toBe(5001);

	// A millisecond past 5000us passes it, though its own timescale is coarser than the track's.
	const later = Time.Timestamp.fromMillis(6);
	expect(next(track, later)).toBe(later);
});

test("next bumps a millisecond write that ties a finer start", () => {
	const track = new Track.Producer("test").accept({ timescale: Time.Timescale.MICRO });
	track.appendGroup({ payload: new Uint8Array(), timestamp: new Time.Timestamp(5_500, Time.Timescale.MICRO) });
	// 5ms floors to 5000us, behind 5500us: in its own scale it would compare 5 against 5500.
	expect(next(track, Time.Timestamp.fromMillis(5)).value).toBe(5501);
});
