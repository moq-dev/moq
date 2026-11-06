//! Timed text reconstructed from container sample metadata.

use moq_net::Timestamp;
use std::ops::Range;

pub(super) struct Cue<'a> {
	pub start: Timestamp,
	pub end: Timestamp,
	pub body: &'a str,
	pub id: &'a str,
	pub settings: &'a str,
}

pub(super) fn stamp(value: Timestamp) -> String {
	format_millis(value.as_micros() / 1000)
}

fn format_millis(millis: u128) -> String {
	format!(
		"{:02}:{:02}:{:02}.{:03}",
		millis / 3_600_000,
		millis / 60_000 % 60,
		millis / 1000 % 60,
		millis % 1000
	)
}

pub(super) fn header(value: &str) -> Result<String, &'static str> {
	let value = newlines(value.trim_start_matches('\u{feff}'));
	let first = value.lines().next().ok_or("missing WEBVTT header")?;
	if first != "WEBVTT" && !first.starts_with("WEBVTT ") && !first.starts_with("WEBVTT\t") {
		return Err("missing WEBVTT header");
	}
	if value.contains("-->") || value.contains("X-TIMESTAMP-MAP") || value.contains('\0') {
		return Err("unsupported WebVTT header");
	}
	Ok(format!("{}\n\n", value.trim_end_matches('\n')))
}

/// The role a normalized WebVTT header's `Kind` metadata declares; subtitles when absent.
pub(super) fn role(header: &str) -> hang::catalog::TextRole {
	let kind = header
		.lines()
		.skip(1)
		.take_while(|line| !line.is_empty())
		.find_map(|line| line.strip_prefix("Kind:"))
		.map(str::trim);
	match kind {
		None | Some("subtitles") => hang::catalog::TextRole::Subtitle,
		Some("captions") => hang::catalog::TextRole::Caption,
		Some(other) => hang::catalog::TextRole::Unknown(other.into()),
	}
}

/// WebVTT accepts CRLF, LF, and lone CR line terminators.
fn newlines(value: &str) -> String {
	value.replace("\r\n", "\n").replace('\r', "\n")
}

pub(super) fn append(output: &mut String, cue: Cue<'_>) -> Result<(), &'static str> {
	if cue.end <= cue.start {
		return Err("cue duration must be positive");
	}
	for field in [cue.id, cue.settings] {
		if field.contains(['\r', '\n', '\0']) || field.contains("-->") {
			return Err("invalid cue identifier or settings");
		}
	}
	let body = newlines(cue.body);
	if body.contains('\0') || body.contains("\n\n") || body.contains("-->") {
		return Err("invalid WebVTT cue text");
	}
	if !cue.id.is_empty() {
		output.push_str(cue.id);
		output.push('\n');
	}
	// WebVTT has millisecond precision: round the end up so a sub-millisecond cue stays positive.
	let start = cue.start.as_micros() / 1000;
	let end = cue.end.as_micros().div_ceil(1000).max(start + 1);
	output.push_str(&format_millis(start));
	output.push_str(" --> ");
	output.push_str(&format_millis(end));
	if !cue.settings.is_empty() {
		output.push(' ');
		output.push_str(cue.settings);
	}
	output.push('\n');
	output.push_str(body.trim_end_matches('\n'));
	output.push_str("\n\n");
	Ok(())
}

/// Whether cue text contains an inline `<timestamp>` tag.
pub(super) fn has_timestamp(body: &str) -> bool {
	body.split('<')
		.skip(1)
		.any(|tag| tag.starts_with(|c: char| c.is_ascii_digit()))
}

/// Rebase inline timestamps from their source origin onto the cue's presentation time.
pub(super) fn rebase(body: &str, cue: Range<Timestamp>, origin: Timestamp) -> Result<String, &'static str> {
	let start = cue.start;
	let end = cue.end;
	let origin = origin.convert(start.scale()).map_err(|_| "inline timestamp overflow")?;
	let mut output = String::new();
	let mut remaining = body;
	let mut previous = start;
	while let Some(open) = remaining.find('<') {
		output.push_str(&remaining[..open + 1]);
		remaining = &remaining[open + 1..];
		if remaining.as_bytes().first().is_some_and(u8::is_ascii_digit) {
			let close = remaining.find('>').ok_or("unterminated inline timestamp")?;
			let value = &remaining[..close];
			let relative = timestamp(value)?;
			let relative = relative
				.convert(start.scale())
				.map_err(|_| "inline timestamp overflow")?;
			let relative = relative
				.checked_sub(origin)
				.map_err(|_| "inline timestamp before cue origin")?;
			let absolute = start.checked_add(relative).map_err(|_| "inline timestamp overflow")?;
			if absolute <= previous || absolute >= end {
				return Err("inline timestamp outside cue");
			}
			previous = absolute;
			output.push_str(&stamp(absolute));
			remaining = &remaining[close..];
		}
	}
	output.push_str(remaining);
	Ok(output)
}

/// Parse a WebVTT timestamp.
pub(super) fn timestamp(value: &str) -> Result<Timestamp, &'static str> {
	let mut parts: Vec<&str> = value.split(':').collect();
	if !(2..=3).contains(&parts.len()) {
		return Err("invalid inline timestamp");
	}
	let seconds = parts.pop().unwrap();
	let (seconds, millis) = seconds.split_once('.').ok_or("invalid inline timestamp")?;
	let number = |value: &str| -> Result<u64, &'static str> {
		if value.len() < 2 || !value.bytes().all(|value| value.is_ascii_digit()) {
			return Err("invalid inline timestamp");
		}
		value.parse().map_err(|_| "inline timestamp overflow")
	};
	if millis.len() != 3 || seconds.len() != 2 {
		return Err("invalid inline timestamp");
	}
	let seconds = number(seconds)?;
	let minutes = parts.pop().unwrap();
	if minutes.len() != 2 {
		return Err("invalid inline timestamp");
	}
	let minutes = number(minutes)?;
	let hours = parts.pop().map(number).transpose()?.unwrap_or(0);
	if seconds >= 60 || minutes >= 60 {
		return Err("invalid inline timestamp");
	}
	let millis = hours
		.checked_mul(3600)
		.and_then(|value| value.checked_add(minutes * 60 + seconds))
		.and_then(|value| value.checked_mul(1000))
		.and_then(|value| value.checked_add(number(millis).ok()?))
		.ok_or("inline timestamp overflow")?;
	Timestamp::from_micros(millis.checked_mul(1000).ok_or("inline timestamp overflow")?)
		.map_err(|_| "inline timestamp overflow")
}

/// Append a validated WebVTT comment block.
pub(super) fn comments(output: &mut String, value: &str) -> Result<(), &'static str> {
	if !value.starts_with("NOTE") || value.contains("-->") || value.contains('\0') {
		return Err("invalid cue comments");
	}
	output.push_str(value.trim_end_matches('\n'));
	output.push_str("\n\n");
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn inline_timestamps_must_advance_inside_the_cue() {
		let start = Timestamp::from_nanos(250_000_000).unwrap();
		let end = Timestamp::from_nanos(1_500_000_000).unwrap();
		for body in [
			"<00:00.000>",
			"<00:01.250>",
			"<00:00.500><00:00.400>",
			"<000:00.500>",
			"<00:60.000>",
			"<00:00.50>",
		] {
			assert!(rebase(body, start..end, Timestamp::ZERO).is_err(), "{body}");
		}
		assert_eq!(
			rebase("<b>text</b><00:00.500>next", start..end, Timestamp::ZERO).unwrap(),
			"<b>text</b><00:00:00.750>next"
		);
	}

	#[test]
	fn headers_and_cue_fields_cannot_inject_timing() {
		for value in [
			"bad",
			"WEBVTT\nX-TIMESTAMP-MAP=LOCAL:00:00:00.000,MPEGTS:0",
			"WEBVTT\n\n00:00:00.000 --> 00:00:01.000",
		] {
			assert!(header(value).is_err());
		}
		let start = Timestamp::from_micros(0).unwrap();
		let end = Timestamp::from_micros(1_000_000).unwrap();
		for (id, settings, body) in [
			("id\ninjected", "", "body"),
			("", "align:start\n", "body"),
			("", "", "a\n\nb"),
		] {
			assert!(
				append(
					&mut String::new(),
					Cue {
						start,
						end,
						id,
						settings,
						body
					}
				)
				.is_err()
			);
		}
	}
}
