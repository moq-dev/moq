import "./highlight";
import { setBasePath } from "@moq/hang-ui/utilities";

setBasePath(__HANG_UI_ASSETS_PATH__);

import "@moq/hang-ui/publish/element";
import HangMeet from "@moq/hang/meet/element";
import HangPublish from "@moq/hang/publish/element";
import HangSupport from "@moq/hang/support/element";

export { HangMeet, HangSupport, HangPublish };
