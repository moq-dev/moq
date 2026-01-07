import { setBasePath } from "@moq/hang-ui/settings";
import "./highlight";

// Set basePath for hang-ui assets
// These need to be copied out of node_modules and served via a public endpoint
setBasePath("/@moq/hang-ui");

import "@moq/hang-ui/watch/element";
import HangSupport from "@moq/hang/support/element";
import HangWatch from "@moq/hang/watch/element";

export { HangSupport, HangWatch };

const watch = document.querySelector("hang-watch") as HangWatch | undefined;
if (!watch) throw new Error("unable to find <hang-watch> element");

// If query params are provided, use it as the broadcast path.
const urlParams = new URLSearchParams(window.location.search);
const path = urlParams.get("path");
if (path) {
	watch.setAttribute("path", path);
}
