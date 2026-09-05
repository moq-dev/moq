//! Replays the golden vectors shared with `js/path`, so the two implementations agree.

use moq_path::{Error, Pattern, Patterns, Segment};
use serde_json::Value;

fn vectors() -> Value {
	serde_json::from_str(include_str!("vectors.json")).expect("vectors.json parses")
}

fn pattern(text: &str) -> Pattern {
	text.parse().unwrap_or_else(|err| panic!("{text:?}: {err}"))
}

fn patterns(list: &Value) -> Patterns {
	list.as_array()
		.unwrap()
		.iter()
		.map(|v| pattern(v.as_str().unwrap()))
		.collect()
}

fn error_code(err: &Error) -> &'static str {
	match err {
		Error::EmptySegment => "empty-segment",
		Error::InvalidLiteral(_) => "invalid-literal",
		Error::MultipleGlobstars => "multiple-globstars",
		Error::TooManySegments => "too-many-segments",
		_ => "unknown",
	}
}

fn segment(value: &Value) -> Segment {
	match value["kind"].as_str().unwrap() {
		"literal" => Segment::Literal(value["value"].as_str().unwrap().to_string()),
		"wildcard" => Segment::Wildcard,
		"globstar" => Segment::Globstar,
		other => panic!("unknown segment kind {other}"),
	}
}

#[test]
fn parse() {
	for case in vectors()["parse"].as_array().unwrap() {
		let text = case["text"].as_str().unwrap();
		match text.parse::<Pattern>() {
			Ok(got) => {
				assert!(case["error"].is_null(), "{text:?} should fail with {}", case["error"]);
				assert_eq!(got.to_string(), text, "{text:?} does not print canonically");
				if let Some(segments) = case["segments"].as_array() {
					let expected: Vec<Segment> = segments.iter().map(segment).collect();
					assert_eq!(got.segments(), expected, "{text:?}");
					assert_eq!(Pattern::new(expected).unwrap(), got, "{text:?} rebuilt from segments");
				}
			}
			Err(err) => assert_eq!(error_code(&err), case["error"].as_str().unwrap_or("ok"), "{text:?}"),
		}
	}
}

#[test]
fn literal_and_subtree() {
	let vectors = vectors();
	for case in vectors["literal"].as_array().unwrap() {
		let path = case["path"].as_str().unwrap();
		match Pattern::literal(path) {
			Ok(got) => assert_eq!(got, pattern(case["pattern"].as_str().unwrap()), "literal {path:?}"),
			Err(err) => assert_eq!(
				error_code(&err),
				case["error"].as_str().unwrap_or("ok"),
				"literal {path:?}"
			),
		}
	}
	for case in vectors["subtree"].as_array().unwrap() {
		let path = case["path"].as_str().unwrap();
		assert_eq!(
			Pattern::subtree(path).unwrap(),
			pattern(case["pattern"].as_str().unwrap()),
			"subtree {path:?}"
		);
	}
}

#[test]
fn head() {
	for case in vectors()["head"].as_array().unwrap() {
		let p = pattern(case["pattern"].as_str().unwrap());
		assert_eq!(p.head(), case["head"].as_str().unwrap(), "{p}");
		assert_eq!(p.is_literal(), case["literal"].as_bool().unwrap(), "{p}");
		assert_eq!(p.has_globstar(), case["globstar"].as_bool().unwrap(), "{p}");
	}
}

#[test]
fn matches() {
	for case in vectors()["matches"].as_array().unwrap() {
		let p = pattern(case["pattern"].as_str().unwrap());
		let path = case["path"].as_str().unwrap();
		assert_eq!(
			p.matches(path),
			case["expect"].as_bool().unwrap(),
			"{p} matches {path:?}"
		);
	}
}

#[test]
fn contains() {
	for case in vectors()["contains"].as_array().unwrap() {
		let outer = pattern(case["outer"].as_str().unwrap());
		let inner = pattern(case["inner"].as_str().unwrap());
		assert_eq!(
			outer.contains(&inner),
			case["expect"].as_bool().unwrap(),
			"{outer} contains {inner}"
		);
	}
}

#[test]
fn overlaps() {
	for case in vectors()["overlaps"].as_array().unwrap() {
		let a = pattern(case["a"].as_str().unwrap());
		let b = pattern(case["b"].as_str().unwrap());
		let expect = case["expect"].as_bool().unwrap();
		assert_eq!(a.overlaps(&b), expect, "{a} overlaps {b}");
		assert_eq!(b.overlaps(&a), expect, "{b} overlaps {a}");
	}
}

#[test]
fn specificity() {
	let vectors = vectors();
	for list in vectors["specificity"]["descending"].as_array().unwrap() {
		let ranked: Vec<Pattern> = list
			.as_array()
			.unwrap()
			.iter()
			.map(|v| pattern(v.as_str().unwrap()))
			.collect();
		for pair in ranked.windows(2) {
			assert!(
				pair[0].specificity() > pair[1].specificity(),
				"{} should outrank {}",
				pair[0],
				pair[1]
			);
		}
	}
	for pair in vectors["specificity"]["equal"].as_array().unwrap() {
		let a = pattern(pair[0].as_str().unwrap());
		let b = pattern(pair[1].as_str().unwrap());
		assert_eq!(a.specificity(), b.specificity(), "{a} and {b} should tie");
	}
}

#[test]
fn rebase() {
	for case in vectors()["rebase"].as_array().unwrap() {
		let p = pattern(case["pattern"].as_str().unwrap());
		let root = case["root"].as_str().unwrap();
		assert_eq!(p.rebase(root), patterns(&case["expect"]), "{p} rebased at {root:?}");
	}
}

#[test]
fn rooted() {
	for case in vectors()["rooted"].as_array().unwrap() {
		let p = pattern(case["pattern"].as_str().unwrap());
		let root = case["root"].as_str().unwrap();
		match p.rooted(root) {
			Ok(got) => assert_eq!(got, pattern(case["expect"].as_str().unwrap()), "{p} rooted at {root:?}"),
			Err(err) => assert_eq!(
				error_code(&err),
				case["error"].as_str().unwrap_or("ok"),
				"{p} rooted at {root:?}"
			),
		}
	}
}

#[test]
fn union() {
	let vectors = vectors();
	for case in vectors["union"].as_array().unwrap() {
		let reduced = patterns(&case["input"]);
		let expected: Vec<Pattern> = case["reduced"]
			.as_array()
			.unwrap()
			.iter()
			.map(|v| pattern(v.as_str().unwrap()))
			.collect();
		assert_eq!(
			reduced.iter().cloned().collect::<Vec<_>>(),
			expected,
			"{}",
			case["input"]
		);
	}
	for case in vectors["unionContains"].as_array().unwrap() {
		let set = patterns(&case["union"]);
		let p = pattern(case["pattern"].as_str().unwrap());
		assert_eq!(
			set.contains(&p),
			case["expect"].as_bool().unwrap(),
			"{} contains {p}",
			case["union"]
		);
	}
}
