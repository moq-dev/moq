// Consume a hang broadcast from a relay and publish a just-in-time transcoded
// derivative next to it.
//
// Publish something first (e.g. `moq import capture` from moq-cli), then:
//
//     cargo run -p moq-transcode --example transcode -- \
//         --url http://localhost:4443/anon --source my-broadcast
//
// When the derivative is nested beneath the source, its catalog references the
// source renditions relatively and adds the ladder rungs. An unrelated output
// omits the passthrough renditions. Rungs are only encoded while watched or fetched.

use anyhow::Context;

#[derive(usage_rs::Cli)]
#[usage(bin = "transcode", unknown_flags = "error", args_override_self = false)]
struct Args {
	/// The relay URL, including any auth path prefix.
	#[usage(long, default = "http://localhost:4443/anon")]
	url: url::Url,

	/// The source broadcast path within the origin.
	#[usage(long)]
	source: String,

	/// The derivative broadcast path. Defaults to `<source>/transcode.hang`.
	#[usage(long)]
	output: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	moq_tokio::Log::new(tracing::Level::INFO).init()?;
	let args = Args::parse();
	let source_path = moq_net::Path::try_from(args.source)?;
	let output_path = moq_net::Path::try_from(
		args.output
			.clone()
			.unwrap_or_else(|| format!("{source_path}/transcode.hang")),
	)?;

	// Publish the derivative through one origin and consume the source through
	// another, over a single auto-reconnecting session.
	let publish = moq_tokio::origin::spawn();
	let remote = moq_tokio::origin::spawn();

	let client = moq_tokio::connect::Config::default().init(Default::default())?;
	let session = client
		.with_publisher(&publish)
		.with_subscriber(remote.clone())
		.connect(args.url.clone());

	// Follow the source until it is announced rather than asking once the session
	// connects: the announcement that makes the path routable may still be in flight.
	//
	// Raced against the session ending, since the wait itself never fails: the origin
	// outlives the session here, so a rejected token or an exhausted retry budget would
	// otherwise leave us waiting for an announcement that can never arrive. The session
	// subscribes upstream on demand.
	let mut following = remote.consume().follow(&source_path, None)?;
	let source = tokio::select! {
		resolved = following.available() => resolved.context("source broadcast unavailable")?,
		closed = session.closed() => {
			closed.context("session failed before the source broadcast was announced")?;
			anyhow::bail!("session closed before the source broadcast was announced");
		}
	};

	// The default ladder and encoder (hardware first: NVENC on Linux) apply.
	let mut config = moq_transcode::Config::default();

	// Point the derivative catalog at the source renditions so players fetch them from the
	// source directly. An empty reference would name the derivative broadcast itself, which
	// publishes the rungs and nothing else.
	config.source = source_path.relative(&output_path).filter(|rel| !rel.is_empty());

	let output = publish
		.create_broadcast(&output_path)
		.context("failed to create the derivative broadcast")?;
	output
		.announce(Default::default())
		.context("failed to announce the derivative broadcast")?;
	tracing::info!(source = %source_path, output = %output_path, "transcoding");

	tokio::select! {
		res = moq_transcode::run(source, output.broadcast().clone(), config) => Ok(res?),
		res = session.closed() => Ok(res?),
	}
}
