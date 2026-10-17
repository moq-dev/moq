//! TTML documents moved from a source's media timeline onto the catalog clock.
//!
//! Only well-formed XML in the timing profile IMSC1 admits is supported: a `tt` root in the TTML
//! namespace, no DTD, `ttp:timeBase="media"`, `par` time containers, and clock or offset time
//! expressions without sub-frames. Anything else is refused rather than relabelled.

use moq_net::Timestamp;
use std::ops::Range;

/// The TTML namespace, which a sample entry must list and a document's root must use.
pub(super) const NAMESPACE: &str = "http://www.w3.org/ns/ttml";
const PARAMETER: &str = "http://www.w3.org/ns/ttml#parameter";

const NANOS: u128 = 1_000_000_000;

/// Rewrite every time expression in a TTML `document` onto the catalog clock.
///
/// The document's times are on the source track's media timeline (ISO/IEC 14496-30), where the
/// sample starts at `source`. The sample spans `sample` on the catalog clock, and the document only
/// presents within it, so every time is also clipped to it: the payload then renders on its own.
pub(super) fn rewrite(document: &[u8], source: Timestamp, sample: Range<Timestamp>) -> Result<String, &'static str> {
	let document = std::str::from_utf8(document).map_err(|_| "invalid TTML UTF-8")?;
	// A strict parse: the output keeps every source byte it does not retime, so it is only as
	// well-formed as the input. DTDs stay refused, since their entities and defaults could change
	// what the attributes say.
	let tree = roxmltree::Document::parse(document).map_err(|_| "malformed TTML document")?;
	// The text was decoded as UTF-8, whatever the (already validated) declaration says.
	let declaration = document
		.trim_start_matches('\u{feff}')
		.strip_prefix("<?xml")
		.and_then(|rest| rest.split_once("?>"))
		.map_or("", |(declaration, _)| declaration);
	if let Some((_, encoding)) = declaration.split_once("encoding") {
		let encoding = encoding.trim_start().trim_start_matches('=').trim_start();
		let quote = encoding.chars().next().unwrap_or('"');
		let name = encoding[quote.len_utf8()..].split(quote).next().unwrap_or_default();
		if !name.eq_ignore_ascii_case("UTF-8") {
			return Err("TTML document is not UTF-8");
		}
	}
	let root = tree.root_element();
	if root.tag_name().namespace() != Some(NAMESPACE) || root.tag_name().name() != "tt" {
		return Err("TTML root is not a tt element in the TTML namespace");
	}
	let rates = Rates::from_root(root)?;
	let clock = Clock {
		offset: sample.start.as_nanos() as i128 - source.as_nanos() as i128,
		start: sample.start.as_nanos(),
		end: sample.end.as_nanos(),
	};
	let mut edit = Edit {
		document,
		edits: Vec::new(),
	};
	// Each element's scope, by node id, so a child finds its parent's; the root's parent begins at
	// zero on both clocks.
	let mut scopes = vec![Scope::default(); tree.descendants().count()];
	for element in root.descendants().filter(|node| node.is_element()) {
		let parent = element
			.parent_element()
			.map_or(Scope::default(), |parent| scopes[parent.id().get_usize()]);
		scopes[element.id().get_usize()] = if element.tag_name().namespace() == Some(NAMESPACE) {
			timing(element, &rates, &clock, parent, &mut edit)?
		} else {
			// Foreign elements are not timed; their content inherits the enclosing begin.
			parent
		};
	}
	Ok(edit.apply())
}

/// Splices replacement text into the original document, keeping every other byte verbatim.
struct Edit<'a> {
	document: &'a str,
	edits: Vec<(Range<usize>, String)>,
}

impl Edit<'_> {
	fn replace(&mut self, value: Range<usize>, nanos: u128) {
		self.edits.push((value, clock_time(nanos)));
	}

	/// Add an attribute right after the element's name.
	fn insert(&mut self, element: roxmltree::Node, name: &str, nanos: u128) {
		let start = element.range().start;
		let tag = &self.document[start..];
		let at = start
			+ tag
				.find(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>')
				.unwrap_or(tag.len());
		self.edits.push((at..at, format!(" {name}=\"{}\"", clock_time(nanos))));
	}

	fn apply(mut self) -> String {
		self.edits.sort_by_key(|(range, _)| range.start);
		let mut output = String::with_capacity(self.document.len());
		let mut copied = 0;
		for (range, value) in self.edits {
			output.push_str(&self.document[copied..range.start]);
			output.push_str(&value);
			copied = range.end;
		}
		output.push_str(&self.document[copied..]);
		output
	}
}

/// An element's begin on both timelines, which its children's times are relative to.
#[derive(Clone, Copy, Default)]
struct Scope {
	source: u128,
	mapped: u128,
	/// Where a child without its own begin starts on the catalog clock. Only the body differs from
	/// `mapped`: it begins at the frame timestamp even when the source started it later.
	implicit: u128,
}

/// Maps a source media time onto the catalog clock, clipped to the sample.
struct Clock {
	offset: i128,
	start: u128,
	end: u128,
}

impl Clock {
	/// Monotonic, so every relative time it rewrites stays non-negative.
	fn map(&self, source: u128) -> u128 {
		// A time past i128 is far past the sample, so saturating keeps the map monotonic.
		i128::try_from(source)
			.unwrap_or(i128::MAX)
			.saturating_add(self.offset)
			.clamp(self.start as i128, self.end as i128) as u128
	}
}

/// Rewrite one TTML element's `begin`, `end`, and `dur`, returning the scope for its children.
///
/// Each is resolved to an absolute source time, mapped, and written back relative to its own
/// reference: `begin` and `end` to the parent's begin, `dur` to the element's begin. The body always
/// begins at the frame timestamp and gains an end, so even untimed content stays inside the sample;
/// a later source begin moves onto its children instead.
fn timing(
	element: roxmltree::Node,
	rates: &Rates,
	clock: &Clock,
	parent: Scope,
	edit: &mut Edit,
) -> Result<Scope, &'static str> {
	let mut begin = None;
	let mut end = None;
	let mut dur = None;
	for attribute in element.attributes().filter(|attribute| attribute.namespace().is_none()) {
		// The raw range is spliced in place; the value, which may differ by character references, is
		// what gets parsed.
		let (raw, value) = (attribute.range_value(), attribute.value());
		match attribute.name() {
			"begin" => begin = Some((raw, value)),
			"end" => end = Some((raw, value)),
			"dur" => dur = Some((raw, value)),
			// `seq` would make each child's begin relative to its previous sibling's end.
			"timeContainer" if value != "par" => return Err("unsupported TTML time container"),
			_ => {}
		}
	}
	// Timing on the root is not TTML, and would give the body a parent begin past its frame.
	if element.tag_name().name() == "tt" && (begin.is_some() || end.is_some() || dur.is_some()) {
		return Err("timing on the TTML root element");
	}
	let body = element.tag_name().name() == "body";

	let offset = begin.as_ref().map(|(_, value)| rates.time(value)).transpose()?;
	let source = parent
		.source
		.checked_add(offset.unwrap_or_default())
		.ok_or("TTML time overflow")?;
	let (mapped, implicit) = match (offset, body) {
		(_, true) => (clock.start, clock.map(source)),
		(None, false) => (parent.implicit, parent.implicit),
		(Some(_), false) => (clock.map(source), clock.map(source)),
	};
	// Only a body nested in timed content can begin before its parent.
	let relative = mapped
		.checked_sub(parent.mapped)
		.ok_or("TTML body inside timed content")?;
	match (begin, offset) {
		(Some((raw, _)), Some(offset)) if offset != relative => edit.replace(raw, relative),
		(None, _) if body || relative != 0 => edit.insert(element, "begin", relative),
		_ => {}
	}

	// A scope's mapped begin never exceeds the mapping of its source begin, so neither subtraction
	// below underflows, even when an element ends before it begins.
	let ends = end.is_some() || dur.is_some();
	if let Some((raw, value)) = end {
		let offset = rates.time(value)?;
		let at = parent.source.checked_add(offset).ok_or("TTML time overflow")?;
		let relative = clock.map(at) - parent.mapped;
		if relative != offset {
			edit.replace(raw, relative);
		}
	}
	if let Some((raw, value)) = dur {
		let offset = rates.time(value)?;
		let at = source.checked_add(offset).ok_or("TTML time overflow")?;
		let relative = clock.map(at) - mapped;
		if relative != offset {
			edit.replace(raw, relative);
		}
	}
	if body && !ends {
		edit.insert(element, "end", clock.end - parent.mapped);
	}

	Ok(Scope {
		source,
		mapped,
		implicit,
	})
}

/// The root's timing parameters, which give frame and tick expressions their length.
struct Rates {
	/// The frame rate, when `ttp:frameRate` declares it.
	frame: Option<FrameRate>,
	/// Ticks per second, when `ttp:tickRate` declares it.
	tick: Option<u128>,
}

impl Rates {
	fn from_root(root: roxmltree::Node) -> Result<Self, &'static str> {
		let mut frame = None;
		let mut multiplier = None;
		let mut tick = None;
		for attribute in root
			.attributes()
			.filter(|attribute| attribute.namespace() == Some(PARAMETER))
		{
			let value = attribute.value();
			match attribute.name() {
				"timeBase" if value != "media" => return Err("unsupported TTML time base"),
				"frameRate" => frame = Some(positive(value)?),
				"frameRateMultiplier" => {
					let mut parts = value.split_whitespace();
					let (Some(num), Some(den), None) = (parts.next(), parts.next(), parts.next()) else {
						return Err("invalid TTML frame rate multiplier");
					};
					multiplier = Some((positive(num)?, positive(den)?));
				}
				"tickRate" => tick = Some(positive(value)?),
				_ => {}
			}
		}
		let frame = match (frame, multiplier) {
			(Some(nominal), multiplier) => {
				let (num, den) = multiplier.unwrap_or((1, 1));
				Some(FrameRate {
					nominal,
					num: nominal.checked_mul(num).ok_or("TTML time overflow")?,
					den,
				})
			}
			(None, Some(_)) => return Err("TTML frame rate multiplier without a frame rate"),
			(None, None) => None,
		};
		Ok(Self { frame, tick })
	}

	/// Parse a time expression into nanoseconds, rounded to the nearest.
	///
	/// IMSC1 requires the rate a frame or tick expression depends on to be declared, so the TTML
	/// defaults are not assumed.
	fn time(&self, value: &str) -> Result<u128, &'static str> {
		const INVALID: &str = "invalid TTML time expression";
		if value.contains(':') {
			let mut parts = value.split(':');
			let (Some(hours), Some(minutes), Some(seconds)) = (parts.next(), parts.next(), parts.next()) else {
				return Err(INVALID);
			};
			let frames = parts.next();
			if parts.next().is_some() || hours.len() < 2 || minutes.len() != 2 {
				return Err(INVALID);
			}
			let (whole, fraction) = seconds.split_once('.').map_or((seconds, None), |(a, b)| (a, Some(b)));
			if whole.len() != 2 || (frames.is_some() && fraction.is_some()) {
				return Err(INVALID);
			}
			let hours = integer(hours)?;
			let minutes = integer(minutes)?;
			let seconds = integer(whole)?;
			if minutes >= 60 || seconds >= 60 {
				return Err(INVALID);
			}
			let whole = hours
				.checked_mul(3600)
				.and_then(|value| value.checked_add(minutes * 60 + seconds))
				.and_then(|value| value.checked_mul(NANOS))
				.ok_or("TTML time overflow")?;
			let part = match (fraction, frames) {
				(Some(fraction), None) => decimal("0", Some(fraction), NANOS, 1)?,
				(None, Some(frames)) => {
					if frames.len() < 2 || frames.contains('.') {
						return Err("unsupported TTML sub-frame time expression");
					}
					let rate = self.frame.as_ref().ok_or("TTML frame time without ttp:frameRate")?;
					let frames = integer(frames)?;
					// The frame field counts nominal frames, whatever the multiplier.
					if frames >= rate.nominal {
						return Err(INVALID);
					}
					let unit = NANOS.checked_mul(rate.den).ok_or("TTML time overflow")?;
					decimal_ratio(frames, 0, unit, rate.num)?
				}
				_ => 0,
			};
			return whole.checked_add(part).ok_or("TTML time overflow");
		}

		let split = value.find(|c: char| !c.is_ascii_digit() && c != '.').ok_or(INVALID)?;
		let (count, metric) = value.split_at(split);
		let (whole, fraction) = count.split_once('.').map_or((count, None), |(a, b)| (a, Some(b)));
		let (num, den) = match metric {
			"h" => (3600 * NANOS, 1),
			"m" => (60 * NANOS, 1),
			"s" => (NANOS, 1),
			"ms" => (NANOS / 1000, 1),
			"f" => {
				let rate = self.frame.as_ref().ok_or("TTML frame time without ttp:frameRate")?;
				(NANOS.checked_mul(rate.den).ok_or("TTML time overflow")?, rate.num)
			}
			"t" => (NANOS, self.tick.ok_or("TTML tick time without ttp:tickRate")?),
			_ => return Err(INVALID),
		};
		decimal(whole, fraction, num, den)
	}
}

/// `ttp:frameRate` and the effective rate its multiplier gives, `num / den` frames per second.
struct FrameRate {
	nominal: u128,
	num: u128,
	den: u128,
}

/// `whole.fraction` times `num / den`, rounded to the nearest.
fn decimal(whole: &str, fraction: Option<&str>, num: u128, den: u128) -> Result<u128, &'static str> {
	let (places, fraction) = match fraction {
		Some(fraction) => (fraction.len() as u32, integer(fraction)?),
		None => (0, 0),
	};
	let mantissa = 10u128
		.checked_pow(places)
		.and_then(|scale| integer(whole).ok()?.checked_mul(scale))
		.and_then(|value| value.checked_add(fraction))
		.ok_or("invalid TTML time expression")?;
	decimal_ratio(mantissa, places, num, den)
}

/// `mantissa / 10^places` times `num / den`, rounded to the nearest.
fn decimal_ratio(mantissa: u128, places: u32, num: u128, den: u128) -> Result<u128, &'static str> {
	let den = 10u128
		.checked_pow(places)
		.and_then(|scale| scale.checked_mul(den))
		.ok_or("TTML time overflow")?;
	let product = mantissa.checked_mul(num).ok_or("TTML time overflow")?;
	Ok(product / den + u128::from(product % den >= den - den / 2))
}

fn integer(value: &str) -> Result<u128, &'static str> {
	if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
		return Err("invalid TTML time expression");
	}
	value.parse().map_err(|_| "TTML time overflow")
}

fn positive(value: &str) -> Result<u128, &'static str> {
	match integer(value) {
		Ok(0) | Err(_) => Err("invalid TTML timing parameter"),
		Ok(value) => Ok(value),
	}
}

/// A TTML clock-time expression, `hh:mm:ss` with only the fraction it needs.
fn clock_time(nanos: u128) -> String {
	let seconds = nanos / NANOS;
	let mut output = format!("{:02}:{:02}:{:02}", seconds / 3600, seconds / 60 % 60, seconds % 60);
	let fraction = nanos % NANOS;
	if fraction != 0 {
		output.push_str(format!(".{fraction:09}").trim_end_matches('0'));
	}
	output
}

#[cfg(test)]
mod tests {
	use super::*;

	fn secs(value: u64) -> Timestamp {
		Timestamp::from_secs(value).unwrap()
	}

	const ROOT: &str = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttp="http://www.w3.org/ns/ttml#parameter""#;

	/// Nested times are relative to their parent's begin, so only the outermost move with the clock,
	/// while content outside the sample is clipped to it.
	#[test]
	fn nested_times_move_onto_a_shifted_clock_and_clip_to_the_sample() {
		let document = format!(
			r#"<?xml version="1.0" encoding="UTF-8"?>
{ROOT} ttp:frameRate="25" ttp:tickRate="10000000">
<head><layout><region xml:id="r1"/></layout></head>
<body><div begin="00:01:41.000"><p begin="1s" end="00:00:02:12" region="r1">Hello <span begin="5000000t" dur="10f">world</span></p></div><p begin="00:01:39" end="105s">Clipped</p></body></tt>"#
		);
		let output = rewrite(document.as_bytes(), secs(100), secs(10)..secs(14)).unwrap();
		assert_eq!(
			output,
			format!(
				r#"<?xml version="1.0" encoding="UTF-8"?>
{ROOT} ttp:frameRate="25" ttp:tickRate="10000000">
<head><layout><region xml:id="r1"/></layout></head>
<body begin="00:00:10" end="00:00:14"><div begin="00:00:01"><p begin="1s" end="00:00:02:12" region="r1">Hello <span begin="5000000t" dur="10f">world</span></p></div><p begin="00:00:00" end="00:00:04">Clipped</p></body></tt>"#
			)
		);
	}

	/// The body begins at the frame timestamp; a later source begin moves onto its untimed children.
	#[test]
	fn body_begins_at_the_frame_and_clips_to_it() {
		let document = format!(r#"{ROOT}><body begin="1.5s" dur="10s"><p>Hi</p></body></tt>"#);
		let output = rewrite(document.as_bytes(), secs(1), secs(1)..secs(3)).unwrap();
		assert_eq!(
			output,
			format!(r#"{ROOT}><body begin="00:00:01" dur="00:00:02"><p begin="00:00:00.5">Hi</p></body></tt>"#)
		);
		// Character references are parsed by value, and an element may end before it begins.
		let document = format!(r#"{ROOT}><body begin="1&#x73;"><p begin="5s" end="4s">Hi</p></body></tt>"#);
		assert_eq!(
			rewrite(document.as_bytes(), secs(1), secs(1)..secs(3)).unwrap(),
			format!(
				r#"{ROOT}><body end="00:00:03" begin="1&#x73;"><p begin="00:00:02" end="00:00:02">Hi</p></body></tt>"#
			)
		);
		// Predefined entities and character references resolve without a DTD.
		let entities = format!(r#"{ROOT}><body begin="00:00:01" end="00:00:03"><p>&lt;&#160;&amp;</p></body></tt>"#);
		assert_eq!(
			rewrite(entities.as_bytes(), secs(1), secs(1)..secs(3)).unwrap(),
			entities
		);
		// A time past i128 clips to the sample's end rather than wrapping before its start.
		let far = format!(
			r#"{ROOT}><body begin="00:00:01" end="00:00:03"><p begin="{}s"/></body></tt>"#,
			i128::MAX as u128 / NANOS + 1
		);
		assert!(
			rewrite(far.as_bytes(), secs(1), secs(1)..secs(3))
				.unwrap()
				.contains(r#"<p begin="00:00:02"/>"#)
		);
		let declared = format!(r#"<?xml version="1.0" encoding='utf-8'?>{entities}"#);
		assert_eq!(
			rewrite(declared.as_bytes(), secs(1), secs(1)..secs(3)).unwrap(),
			declared
		);
		// A byte order mark is not text outside the root.
		let marked = format!("\u{feff}{document}");
		assert_eq!(
			rewrite(marked.as_bytes(), secs(1), secs(1)..secs(3)).unwrap(),
			format!(
				"\u{feff}{}",
				rewrite(document.as_bytes(), secs(1), secs(1)..secs(3)).unwrap()
			)
		);
		// An empty document clears the captions.
		let empty = format!("{ROOT}/>");
		assert_eq!(rewrite(empty.as_bytes(), secs(1), secs(1)..secs(3)).unwrap(), empty);
	}

	#[test]
	fn time_expressions() {
		let rates = Rates {
			frame: Some(FrameRate {
				nominal: 30,
				num: 30_000,
				den: 1001,
			}),
			tick: Some(90_000),
		};
		for (value, nanos) in [
			("1.5h", 5_400 * NANOS),
			("2m", 120 * NANOS),
			("0.25s", 250_000_000),
			("1500ms", 1_500_000_000),
			("90000t", NANOS),
			("1t", 11_111),
			("30f", 1_001_000_000),
			("00:00:01.25", 1_250_000_000),
			("100:00:00", 360_000 * NANOS),
			("00:00:01:15", 1_500_500_000),
		] {
			assert_eq!(rates.time(value), Ok(nanos), "{value}");
		}
		for value in [
			"",
			"1",
			"s",
			".5s",
			"1.s",
			"1x",
			"-1s",
			" 1s",
			"0:00:01",
			"00:0:01",
			"00:60:00",
			"00:00:60",
			"00:00:01:30",
			"00:00:01:01.1",
			"00:00:01.5:01",
			"00:00:01:1",
		] {
			assert!(rates.time(value).is_err(), "{value}");
		}
		let huge = Rates {
			frame: Some(FrameRate {
				nominal: 100,
				num: u128::MAX,
				den: u128::MAX,
			}),
			tick: None,
		};
		assert!(huge.time("00:00:00:99").is_err());
		// The frame field is bounded by the nominal rate, not the effective one.
		let halved = Rates {
			frame: Some(FrameRate {
				nominal: 30,
				num: 30,
				den: 2,
			}),
			tick: None,
		};
		assert_eq!(halved.time("00:00:00:20"), Ok(1_333_333_333));
		let doubled = Rates {
			frame: Some(FrameRate {
				nominal: 30,
				num: 60,
				den: 1,
			}),
			tick: None,
		};
		assert!(doubled.time("00:00:00:45").is_err());
		let unset = Rates {
			frame: None,
			tick: None,
		};
		assert!(unset.time("1f").is_err());
		assert!(unset.time("00:00:00:01").is_err());
		assert!(unset.time("1t").is_err());
	}

	#[test]
	fn clock_time_trims_the_fraction() {
		assert_eq!(clock_time(0), "00:00:00");
		assert_eq!(clock_time(3_723_500_000_000), "01:02:03.5");
		assert_eq!(clock_time(360_000 * NANOS + 1), "100:00:00.000000001");
	}

	#[test]
	fn documents_outside_the_profile_are_refused() {
		for document in [
			String::new(),
			"<tt/>".into(),
			r#"<tt xmlns="urn:example"/>"#.into(),
			r#"<p xmlns="http://www.w3.org/ns/ttml"/>"#.into(),
			format!(r#"{ROOT} ttp:timeBase="smpte"/>"#),
			format!(r#"{ROOT} ttp:frameRateMultiplier="1000 1001"/>"#),
			format!(r#"{ROOT} ttp:frameRate="0"/>"#),
			format!(r#"{ROOT}><body timeContainer="seq"/></tt>"#),
			format!(r#"{ROOT} begin="1s"><body/></tt>"#),
			format!(r#"{ROOT}><head><region begin="5s"><body/></region></head></tt>"#),
			format!(r#"{ROOT}><body><p begin="1f"/></body></tt>"#),
			format!(r#"{ROOT}><body><p begin="wallclock(2026-01-01)"/></body></tt>"#),
			format!(r#"{ROOT}><body></tt>"#),
			format!(r#"{ROOT}/>{ROOT}/>"#),
			format!(r#"{ROOT}/>trailing"#),
			format!("<!DOCTYPE tt>{ROOT}/>"),
			format!(r#"{ROOT}><body><p>a&nbsp;b</p></body></tt>"#),
			format!(r#"{ROOT}><body><p xml:id="&nbsp;"/></body></tt>"#),
			format!(r#"{ROOT}><body><bad:p/></body></tt>"#),
			format!(r#"{ROOT}><body><p bad:begin="1s"/></body></tt>"#),
			format!(r#"<![CDATA[x]]>{ROOT}/>"#),
			format!(r#"{ROOT}><!-- a--b --></tt>"#),
			format!(r#"<?xml version="1.0" encoding="UTF-16"?>{ROOT}/>"#),
			format!(r#"<?xml version="1.0" encoding = 'latin1' ?>{ROOT}/>"#),
		] {
			assert!(
				rewrite(document.as_bytes(), secs(1), secs(1)..secs(2)).is_err(),
				"{document}"
			);
		}
		assert!(rewrite(b"\xff", secs(1), secs(1)..secs(2)).is_err());
	}
}
