// Consume a hang broadcast from a relay and publish a just-in-time transcoded
// derivative next to it.
//
// Publish something first (e.g. `moq publish camera` from moq-cli), then:
//
//     cargo run -p moq-transcode --example transcode -- \
//         --url http://localhost:4443/anon --source my-broadcast
//
// When the derivative is nested beneath the source, its catalog references the
// source renditions relatively and adds the ladder rungs. An unrelated output
// omits the passthrough renditions. Rungs are only encoded while watched or fetched.

use anyhow::Context;
use clap::Parser;

#[derive(Parser)]
struct Args {
	/// The relay URL, including any auth path prefix.
	#[arg(long, default_value = "http://localhost:4443/anon")]
	url: url::Url,

	/// The source broadcast path within the origin.
	#[arg(long)]
	source: String,

	/// The derivative broadcast path. Defaults to `<source>/transcode.hang`.
	#[arg(long)]
	output: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	moq_tokio::Log::new(tracing::Level::INFO).init()?;
	let args = Args::parse();
	let source_path = moq_net::PathOwned::from(args.source);
	let output_path = moq_net::PathOwned::from(
		args.output
			.clone()
			.unwrap_or_else(|| format!("{source_path}/transcode.hang")),
	);

	// Publish the derivative through one origin and consume the source through
	// another, over a single auto-reconnecting session.
	let publish = moq_tokio::origin::spawn(moq_net::Origin::random());
	let remote = moq_tokio::origin::spawn(moq_net::Origin::random());

	let client = moq_tokio::connect::Config::default().init(Default::default())?;
	let session = client
		.with_publisher(&publish)
		.with_subscriber(remote.clone())
		.connect(args.url.clone());

	// Wait for the source to be announced rather than for the session to connect:
	// `request_broadcast` answers on the spot, so asking the moment a session exists
	// races the announcement that makes the path routable.
	//
	// Raced against the session ending, since the wait itself never fails: the origin
	// outlives the session here, so a rejected token or an exhausted retry budget would
	// otherwise leave us waiting for an announcement that can never arrive.
	let consumer = remote.consume();
	tokio::select! {
		announced = consumer.announced_broadcast(&source_path) => {
			announced.context("origin closed before the source broadcast was announced")?;
		}
		closed = session.closed() => {
			closed.context("session failed before the source broadcast was announced")?;
			anyhow::bail!("session closed before the source broadcast was announced");
		}
	}

	// Resolve it for real; the session subscribes upstream on demand.
	let source = consumer
		.request_broadcast(&source_path)
		.await
		.context("source broadcast unavailable")?;

	// The default ladder and encoder (hardware first: NVENC on Linux) apply.
	let mut config = moq_transcode::Config::default();

	// Point the derivative catalog at the source renditions so players fetch them from the
	// source directly. An empty reference would name the derivative broadcast itself, which
	// publishes the rungs and nothing else.
	config.source = source_path.relative(&output_path).filter(|rel| !rel.is_empty());

	let output = publish
		.create_broadcast(&output_path, moq_net::broadcast::Route::new().with_announce(true))
		.context("failed to create the derivative broadcast")?;
	tracing::info!(source = %source_path, output = %output_path, "transcoding");

	tokio::select! {
		res = moq_transcode::run(source, output, config) => Ok(res?),
		res = session.closed() => Ok(res?),
	}
}
