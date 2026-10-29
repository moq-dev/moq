import { Arrivals } from "../dist/arrivals.js";
import type { Audio, Arrivals as PublicArrivals } from "../dist/index.js";

// @ts-expect-error A decoder owns an arrivals handle's queue.
new Arrivals();
// @ts-expect-error Construction stays private even with arguments.
new Arrivals({ queue: [], head: 0, closed: false }, new Set());

declare const decoder: Audio.Decoder;
const arrivals: PublicArrivals = decoder.arrivals();
void arrivals.next();
arrivals.close();
