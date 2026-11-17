import MoqPublishSupport, { type Report, type Show, Support } from "../dist/support/element.js";

// The banner's types ship in this package, though the private @moq/ui they come from does not.
class Custom extends Support {
	protected async detect(): Promise<Report> {
		return { summary: "partial", rows: [["Capture", "Video", { hardware: false, software: true }]] };
	}
}

const show: Show = "error";
const banner: Support = new MoqPublishSupport();
banner.show = show;
new Custom().details = true;
