import { expect, test } from "bun:test";
import { Duration } from "@moq/clock";
import { Time, Track } from "@moq/net";
import { MAX_AGE, trackInfo } from "./track";

test("trackInfo declares the media max age, null is unlimited, and a value is kept", async () => {
	expect(MAX_AGE).toEqual(Duration.millis(30_000));
	const omitted = trackInfo({ priority: 1 });
	expect(omitted).toEqual({ timescale: Time.Timescale.MICRO, maxAge: MAX_AGE, priority: 1 });
	expect(trackInfo({ priority: 1, maxAge: undefined }).maxAge).toBe(MAX_AGE);
	expect(trackInfo({ priority: 1, maxAge: null }).maxAge).toBeNull();
	expect(trackInfo({ priority: 1, maxAge: Duration.millis(5000) }).maxAge).toEqual(Duration.millis(5000));

	// What a producer accepting each commits.
	const committed = async (maxAge?: Duration | null) =>
		(await new Track.Producer("media").accept(trackInfo({ priority: 1, maxAge })).info()).maxAge;
	expect(await committed()).toEqual(Duration.millis(30_000));
	expect(await committed(null)).toBeUndefined();
	expect(await committed(Duration.millis(5000))).toEqual(Duration.millis(5000));
});
