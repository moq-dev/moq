// Group-only reads of an ordered track, for tests that check what it delivers rather than the
// gaps it reports.
import type * as Group from "./group.ts";
import type * as Track from "./track.ts";

/** The next group `ordered` returns, past any gap it reports first. */
export async function nextGroup(ordered: Track.Ordered): Promise<Group.Consumer | undefined> {
	for (;;) {
		const item = await ordered.nextGroup();
		if (item?.kind !== "gap") return item?.group;
	}
}

/** The next group `ordered` holds now, past any gap, without waiting. */
export function tryNextGroup(ordered: Track.Ordered): Group.Consumer | undefined {
	for (;;) {
		const item = ordered.tryNextGroup();
		if (item?.kind !== "gap") return item?.group;
	}
}
