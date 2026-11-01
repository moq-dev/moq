import * as Publish from "../dist/index.js";
import { Broadcast, Rendition } from "../dist/index.js";

// @ts-expect-error A broadcast owns each rendition's track and teardown.
new Rendition();
// @ts-expect-error Construction stays private even with arguments.
new Rendition(
	"video/test",
	"video",
	new Publish.Signals.Signal<Publish.Net.Track.Producer | undefined>(undefined),
	() => {},
);
// @ts-expect-error The internal factory is not a package export.
Publish.makeRendition;

const broadcast = new Broadcast();
const rendition: Rendition<Publish.Hang.Catalog.VideoConfig> = broadcast.video("test");
rendition.close();
broadcast.close();
