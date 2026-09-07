/**
 * The command channel between the media driver and the page.
 *
 * Each command is a DOM edit plus the bookkeeping that has to happen with it, which is why the page
 * owns them rather than the driver reaching in. Kept free of side effects so the driver can import
 * the name without pulling the page's role logic into a Bun process.
 *
 * @module
 */

/** Commands a page publishes, each role providing the subset that applies to it. */
export type SmokeControl = {
	/** Stop the fixture publisher, releasing its session. */
	stop(): void;
	/** Start (or restart) the fixture publisher on the same broadcast path. */
	start(): void;
	/** Remove the player from the DOM. */
	detach(): void;
	/** Put the player back and resume sampling. */
	reattach(): void;
	/** Detach the player but leave a second one connected: the leaked-session negative control. */
	detachLeaky(): void;
};

/** The `window` property the commands are published on. */
export const CONTROL = "__moqSmoke";

/** Publish the commands a role supports. */
export function publish(commands: Partial<SmokeControl>): void {
	(window as unknown as Record<string, unknown>)[CONTROL] = commands;
}
