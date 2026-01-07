import { setBasePath } from "@moq/hang-ui/settings";
import "./highlight";

// Set basePath for hang-ui assets
setBasePath("/@moq/hang-ui");

import "@moq/hang-ui/publish/element";
import HangMeet from "@moq/hang/meet/element";
import HangPublish from "@moq/hang/publish/element";
import HangSupport from "@moq/hang/support/element";

export { HangMeet, HangSupport, HangPublish };
