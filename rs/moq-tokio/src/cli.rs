//! Answering a Usage parse result when the generated `parse()` cannot own the process.
//!
//! [`usage::render_failure`] renders [`usage::Error::Help`], `HelpAll` and `Version`
//! as an empty string: they are questions rather than failures, and the generated
//! `parse()` is expected to take them first. A binary that parses more than once
//! never reaches that code, so it has to answer them itself. Both shapes exist here:
//! a TOML merge that layers CLI, env, and file with recorded provenance, and
//! moq-cli's repeated `--` stage grammar.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;
use usage::config::{
	CliLayer, EnvLayer, FileScope, Layer, LayerCtx, LayerError, LayerOutput, Layers, Origin, Registry, Resolved,
	SourceKind, Value, resolve,
};

/// What a Usage parse result asks the process to do.
#[non_exhaustive]
pub enum Answer {
	/// Someone asked a question. Print it to stdout and exit 0.
	Question(String),
	/// The command line was wrong. Print it to stderr and exit 2, as clap does.
	Failure(String),
}

impl Answer {
	/// The text to print.
	pub fn message(&self) -> &str {
		match self {
			Self::Question(text) | Self::Failure(text) => text,
		}
	}

	/// Whether this is a question rather than a failure.
	pub fn is_question(&self) -> bool {
		matches!(self, Self::Question(_))
	}

	/// Print to the right stream and exit with the matching status.
	pub fn exit(self) -> ! {
		match self {
			Self::Question(text) => {
				print!("{text}");
				std::process::exit(0)
			}
			Self::Failure(text) => {
				eprint!("{text}");
				std::process::exit(2)
			}
		}
	}
}

/// Render a Usage parse error into the output and exit status it asks for.
///
/// `root` is the command the spec is rooted at, which a help request needs in order
/// to render the page for the route the words actually took.
pub fn answer(
	spec: &usage::argv::spec::Spec<'_>,
	root: &usage::Command<'_>,
	argv: &[&OsStr],
	err: usage::Error<'_, '_>,
) -> Answer {
	use usage::help::{Page, Style};

	let page = |cmd, want, style| usage::help::page(spec, root, argv, cmd, want, style).unwrap_or_default();

	match err {
		usage::Error::Help { cmd, long } => {
			let want = if long { Page::Long } else { Page::Short };
			Answer::Question(page(cmd, want, Style::auto()))
		}
		usage::Error::HelpAll { cmd } => Answer::Question(page(cmd, Page::All, Style::auto())),
		// Not a request: `arg_required_else_help` found nothing to do. clap prints the
		// short page to stderr and exits 2, and so does this.
		usage::Error::MissingArgsHelp { cmd } => Answer::Failure(page(cmd, Page::Short, Style::auto_stderr())),
		usage::Error::Version { long } => {
			let bin = spec.bin.unwrap_or(spec.name);
			let version = if long {
				spec.long_version.or(spec.version)
			} else {
				spec.version.or(spec.long_version)
			}
			.unwrap_or_default();
			Answer::Question(format!("{bin} {version}\n"))
		}
		err => Answer::Failure(usage::render_failure(spec, argv, &err)),
	}
}

/// Merge CLI, environment, and an optional TOML file with recorded provenance.
///
/// Precedence is CLI > env > file > defaults, declared here: [`Layers`] is
/// highest-first, and a key the command line or the environment actually supplied
/// is never taken from the file. Presence comes from [`CliLayer`] / [`EnvLayer`],
/// not from whether a standing value looks empty, so a file that sets a list to
/// `[]` or a bool to `false` survives.
///
/// `parsed` is the struct Usage filled from argv+env+defaults. File keys that
/// neither the command line nor the environment set replace those defaults.
/// `keep` copies fields a TOML round-trip would drop (hidden CLI-only legacy).
pub fn merge<T>(
	registry: Registry,
	parsed: T,
	cli: &CliLayer,
	env: &EnvLayer,
	file: Option<FileSource<'_>>,
	keep: impl Fn(&mut T, &T),
) -> Result<(T, Resolved), String>
where
	T: Serialize + DeserializeOwned,
{
	let file_layer = file.map(|source| TomlLayer {
		path: source.path,
		value: source.value,
	});
	let mut layers = Layers::new().then(cli).then(env);
	if let Some(ref file) = file_layer {
		layers = layers.then(file);
	}
	let resolved = resolve(registry, layers).map_err(|err| err.to_string())?;

	let occupied = occupied_keys(registry, &resolved);
	let original = parsed;
	let mut merged = toml::Value::try_from(&original).map_err(|err| err.to_string())?;
	if let Some(source) = file {
		overlay_unoccupied(&mut merged, source.value, "", &occupied);
	}
	let mut config: T = merged.try_into().map_err(|err: toml::de::Error| err.to_string())?;
	keep(&mut config, &original);
	Ok((config, resolved))
}

/// A TOML document to merge, named for provenance.
#[derive(Clone, Copy)]
pub struct FileSource<'a> {
	/// Path shown in origin descriptions.
	pub path: &'a Path,
	/// Already-parsed document, aliases already normalized.
	pub value: &'a toml::Value,
}

fn occupied_keys(registry: Registry, resolved: &Resolved) -> HashSet<String> {
	let mut keys: HashSet<String> = registry
		.props
		.iter()
		.filter(|meta| {
			matches!(
				resolved.origin_key(meta.key).map(|origin| origin.kind),
				Some(kind) if kind == SourceKind::CLI || kind == SourceKind::ENV
			)
		})
		.map(|meta| meta.key.to_string())
		.collect();
	// The dial group is `connect.*` in the registry and `client.*` on some serde
	// structs (moq-bench). Occupying one occupies the other so a CLI flag still
	// beats a file key written under the other name.
	let aliases: Vec<String> = keys
		.iter()
		.filter_map(|key| {
			key.strip_prefix("connect.")
				.map(|rest| format!("client.{rest}"))
				.or_else(|| key.strip_prefix("client.").map(|rest| format!("connect.{rest}")))
		})
		.collect();
	keys.extend(aliases);
	keys
}

fn overlay_unoccupied(base: &mut toml::Value, overlay: &toml::Value, path: &str, occupied: &HashSet<String>) {
	if !path.is_empty() && occupied.contains(path) {
		return;
	}
	match (base, overlay) {
		(toml::Value::Table(base), toml::Value::Table(overlay)) => {
			for (key, value) in overlay {
				let child = if path.is_empty() {
					key.clone()
				} else {
					format!("{path}.{key}")
				};
				match base.get_mut(key) {
					Some(existing) => overlay_unoccupied(existing, value, &child, occupied),
					None if occupied.contains(child.as_str()) => {}
					None => {
						if value.is_table() && occupied.iter().any(|key| key.starts_with(&format!("{child}."))) {
							base.insert(key.clone(), toml::Value::Table(toml::Table::new()));
							overlay_unoccupied(base.get_mut(key).expect("just inserted"), value, &child, occupied);
						} else {
							base.insert(key.clone(), value.clone());
						}
					}
				}
			}
		}
		(base, overlay) => *base = overlay.clone(),
	}
}

/// A TOML document as a [`usage::config`] layer: every key present in the file,
/// including empty lists and `false` booleans.
struct TomlLayer<'a> {
	path: &'a Path,
	value: &'a toml::Value,
}

impl Layer for TomlLayer<'_> {
	fn source(&self) -> SourceKind {
		SourceKind::FILE
	}

	fn load(&self, ctx: &LayerCtx) -> Result<LayerOutput, LayerError> {
		let mut out = LayerOutput::new();
		flatten_file(self.path, String::new(), self.value, ctx, &mut out, &|key| {
			ctx.registry().names_file_value(key)
		});
		Ok(out)
	}
}

fn flatten_file(
	path: &Path,
	prefix: String,
	value: &toml::Value,
	ctx: &LayerCtx,
	out: &mut LayerOutput,
	names: &dyn Fn(&str) -> bool,
) {
	match value {
		toml::Value::Table(_) if !prefix.is_empty() && names(&prefix) => {
			push_shaped(path, prefix, table_value(value), ctx, out);
		}
		toml::Value::Table(table) => {
			for (key, inner) in table {
				let child = if prefix.is_empty() {
					key.clone()
				} else {
					format!("{prefix}.{key}")
				};
				flatten_file(path, child, inner, ctx, out, names);
			}
		}
		toml::Value::Array(_) => push_shaped(path, prefix, table_value(value), ctx, out),
		scalar => {
			let origin = Origin::file(format!("{}#{prefix}", path.display()), FileScope::Project);
			match ctx.entry_for_key(&prefix, &scalar_text(scalar), origin) {
				Ok(entry) => out.push(entry),
				Err(warning) => out.warn(warning),
			}
		}
	}
}

fn push_shaped(path: &Path, key: String, value: Value, ctx: &LayerCtx, out: &mut LayerOutput) {
	let origin = Origin::file(format!("{}#{key}", path.display()), FileScope::Project);
	match ctx.entry_from_value(&key, value, origin) {
		Ok(entry) => out.push(entry),
		Err(warning) => out.warn(warning),
	}
}

fn table_value(value: &toml::Value) -> Value {
	match value {
		toml::Value::Table(table) => Value::Map(
			table
				.iter()
				.map(|(key, inner)| (key.clone(), table_value(inner)))
				.collect(),
		),
		toml::Value::Array(items) => Value::List(items.iter().map(table_value).collect()),
		toml::Value::Boolean(flag) => Value::Bool(*flag),
		toml::Value::Integer(int) => Value::Int(*int),
		toml::Value::Float(float) => Value::Float(*float),
		other => Value::String(scalar_text(other)),
	}
}

fn scalar_text(value: &toml::Value) -> String {
	match value {
		toml::Value::String(text) => text.clone(),
		other => other.to_string(),
	}
}
