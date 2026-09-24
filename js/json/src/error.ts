/** A compressed stream frame was encoded but never written. */
export class Desync extends Error {
	constructor() {
		super("compression desynchronized: a record was encoded but never written");
		this.name = "Desync";
	}
}

/** A merge patch arrived before any snapshot. */
export class MissingSnapshot extends Error {
	constructor() {
		super("delta before snapshot");
		this.name = "MissingSnapshot";
	}
}
