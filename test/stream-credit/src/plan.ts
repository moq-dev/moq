/** What the driver hands the page, and what the page reports back. @module */

/** One track the page subscribes to. */
export interface Track {
	name: string;
	/** The subscriber priority, higher first. */
	priority: number;
	/** How long, in milliseconds, a superseded group stays worth reading; zero reads the live edge. */
	budget: number;
}

/** The page's whole job. */
export interface Plan {
	/** The relay, over WebTransport. */
	url: string;
	version: string;
	broadcast: string;
	tracks: Track[];
}

/** One group the reader was handed. */
export interface Group {
	sequence: number;
	/** Each frame's payload, in the order read. */
	frames: string[];
	/** Milliseconds after subscribing that the group's last frame was read. */
	at?: number;
	/** Why reading its frames stopped early. */
	error?: string;
}

/** What one subscription read. */
export interface Received {
	name: string;
	groups: Group[];
	/** Why the subscription ended with an error, or `null` for a clean end. */
	ended: string | null;
	/** Milliseconds after subscribing that it ended, once it has. */
	at?: number;
}

/** What the page read. */
export interface Report {
	version: string;
	tracks: Received[];
	/** The session's close error, or `null` for a clean close. */
	closed: string | null;
}
