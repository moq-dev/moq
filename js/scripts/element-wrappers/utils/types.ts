/**
 * Type definitions for Custom Elements Manifest
 */

export interface Slot {
	name?: string;
	description?: string;
}

export interface EventDeclaration {
	name: string;
	description?: string;
	type?: { text: string };
}

export interface Attribute {
	name: string;
	description?: string;
	type?: { text: string };
}

export interface Property {
	name: string;
	description?: string;
	type?: { text: string };
	readonly?: boolean;
}

export interface Declaration {
	kind: string;
	name: string;
	tagName?: string;
	summary?: string;
	description?: string;
	slots?: Slot[];
	events?: EventDeclaration[];
	attributes?: Attribute[];
	properties?: Property[];
}

export interface Module {
	kind: string;
	path: string;
	declarations?: Declaration[];
	exports?: Array<{ kind: string; name: string }>;
}

export interface CustomElementsManifest {
	schemaVersion: string;
	readme?: string;
	modules: Module[];
}

export interface CustomElement {
	tagName: string;
	className: string;
	summary?: string;
	description?: string;
	slots?: Slot[];
	events?: EventDeclaration[];
	attributes?: Attribute[];
	properties?: Property[];
	examples?: Record<string, string>;
}
