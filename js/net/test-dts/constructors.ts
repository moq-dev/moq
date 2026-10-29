import { Broadcast } from "../dist/index.js";
import { Lister } from "../dist/listing.js";
import { wireOf } from "../dist/wire.js";

// @ts-expect-error A broadcast owns a lister's state.
new Lister();
declare const state: never;
// @ts-expect-error Construction stays private even with arguments.
new Lister(state);
// @ts-expect-error The internal factory is not a package export.
Broadcast.makeLister;

const broadcast = new Broadcast.Producer();
const lister: Lister = wireOf(broadcast).lister();
lister.synced();
lister.close();
broadcast.close();
