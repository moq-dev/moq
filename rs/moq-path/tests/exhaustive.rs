//! Checks the algebra against brute-force matching over every small pattern and path.
//!
//! `contains`, `overlaps`, `rebase`, `specificity`, and union reduction all have a
//! definition in terms of `matches`; this enumerates the small cases and holds each to
//! it. The bounds are chosen so every counterexample the structural rules could miss
//! shows up: a witness path for two patterns of up to three segments never needs more
//! than six.

use moq_path::{Pattern, Patterns, Segment};

const ALPHABET: &[&str] = &["a", "b", "c"];
const MAX_PATTERN: usize = 3;
const MAX_PATH: usize = 6;

/// Every valid pattern over `a`, `b`, `*`, `**` with up to `MAX_PATTERN` segments.
fn patterns() -> Vec<Pattern> {
	let choices = [
		Segment::Literal("a".into()),
		Segment::Literal("b".into()),
		Segment::Wildcard,
		Segment::Globstar,
	];
	let mut out = vec![Pattern::default()];
	let mut layer: Vec<Vec<Segment>> = vec![vec![]];
	for _ in 0..MAX_PATTERN {
		let mut next = Vec::new();
		for prefix in &layer {
			for choice in &choices {
				let mut segments = prefix.clone();
				segments.push(choice.clone());
				if let Ok(pattern) = Pattern::new(segments.clone()) {
					out.push(pattern);
					next.push(segments);
				}
			}
		}
		layer = next;
	}
	out
}

/// Every path over the alphabet with up to `max` segments, the empty path included.
fn paths(max: usize) -> Vec<String> {
	let mut out = vec![String::new()];
	let mut layer = vec![Vec::<&str>::new()];
	for _ in 0..max {
		let mut next = Vec::new();
		for prefix in &layer {
			for part in ALPHABET {
				let mut parts = prefix.clone();
				parts.push(part);
				out.push(parts.join("/"));
				next.push(parts);
			}
		}
		layer = next;
	}
	out
}

#[test]
fn contains_means_every_match_is_shared() {
	let paths = paths(MAX_PATH);
	for a in patterns() {
		for b in patterns() {
			let expected = paths.iter().all(|p| !b.matches(p) || a.matches(p));
			assert_eq!(a.contains(&b), expected, "{a} contains {b}");
		}
	}
}

#[test]
fn overlaps_means_some_match_is_shared() {
	let paths = paths(MAX_PATH);
	for a in patterns() {
		for b in patterns() {
			let expected = paths.iter().any(|p| a.matches(p) && b.matches(p));
			assert_eq!(a.overlaps(&b), expected, "{a} overlaps {b}");
		}
	}
}

#[test]
fn specificity_agrees_with_strict_containment() {
	for a in patterns() {
		for b in patterns() {
			if a.contains(&b) && !b.contains(&a) {
				assert!(
					a.specificity() < b.specificity(),
					"{a} strictly contains {b} but does not rank below it"
				);
			}
		}
	}
}

#[test]
fn rebase_matches_exactly_what_lies_beneath_the_root() {
	let roots = paths(3);
	let relative = paths(4);
	for pattern in patterns() {
		for root in &roots {
			let rebased = pattern.rebase(root);
			for rel in &relative {
				let full = if root.is_empty() {
					rel.clone()
				} else if rel.is_empty() {
					root.clone()
				} else {
					format!("{root}/{rel}")
				};
				assert_eq!(
					rebased.matches(rel),
					pattern.matches(&full),
					"{pattern} rebased at {root:?} on {rel:?}"
				);
			}
			// The rebased union is reduced: no member contains another.
			for a in rebased.iter() {
				for b in rebased.iter() {
					assert!(a == b || !a.contains(b), "{rebased:?} is not reduced");
				}
			}
		}
	}
}

#[test]
fn rooted_inverts_rebase() {
	for pattern in patterns() {
		for root in paths(2) {
			let rooted = pattern.rooted(&root).unwrap();
			assert!(rooted.rebase(&root).contains(&pattern), "{pattern} under {root:?}");
			for p in paths(4) {
				let expect = pattern.matches(&p);
				let full = if root.is_empty() { p.clone() } else if p.is_empty() { root.clone() } else { format!("{root}/{p}") };
				assert_eq!(rooted.matches(&full), expect, "{rooted} on {full:?}");
			}
		}
	}
}

#[test]
fn union_matches_exactly_its_members() {
	let all = patterns();
	let paths = paths(4);
	// Every pair and triple, which is enough to exercise reduction in both directions.
	for (i, a) in all.iter().enumerate() {
		for b in &all[i..] {
			let set: Patterns = [a.clone(), b.clone()].into_iter().collect();
			for p in &paths {
				assert_eq!(set.matches(p), a.matches(p) || b.matches(p), "{{{a}, {b}}} on {p:?}");
			}
			assert!(set.len() <= 2);
			assert_eq!(set.len() == 1, a.contains(b) || b.contains(a), "{{{a}, {b}}} reduction");
		}
	}
}

#[test]
fn text_round_trips_through_parse() {
	for pattern in patterns() {
		let text = pattern.to_string();
		assert_eq!(text.parse::<Pattern>().unwrap(), pattern);
		assert_eq!(Pattern::new(pattern.segments().to_vec()).unwrap(), pattern);
	}
}

/// Random strings, parsed and printed, to catch a parse that accepts what print
/// cannot reproduce. A tiny xorshift keeps the crate free of a dependency.
#[test]
fn random_text_round_trips() {
	let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
	let mut next = move || {
		state ^= state << 13;
		state ^= state >> 7;
		state ^= state << 17;
		state
	};
	const PIECES: &[&str] = &["a", "b", "*", "**", "/", "", "a*", ".hang", "//"];
	for _ in 0..20_000 {
		let len = (next() % 8) as usize;
		let text: String = (0..len).map(|_| PIECES[(next() % PIECES.len() as u64) as usize]).collect();
		if let Ok(pattern) = text.parse::<Pattern>() {
			assert_eq!(pattern.to_string(), text, "parse accepted a non-canonical text");
			assert_eq!(text.parse::<Pattern>().unwrap(), pattern);
			assert!(pattern.contains(&pattern) && pattern.overlaps(&pattern));
		}
	}
}
