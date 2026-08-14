// Consume a hang broadcast from a relay and publish a just-in-time transcoded
// derivative next to it.
//
// Publish something first (e.g. `moq publish camera` from moq-cli), then:
//
//     cargo run -p moq-transcode --example transcode -- \
//         --url http://localhost:4443/anon --source my-broadcast
//
// The derivative appears at `<source>/transcode.hang`: its catalog references
// the source renditions via a relative `broadcast: ".."` pointer and adds the
// ladder rungs, which are only encoded while someone watches (or fetches) them.

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
	moq_native::Log::new(tracing::Level::INFO).init()?;
	let args = Args::parse();
	let output_path = args
		.output
		.clone()
		.unwrap_or_else(|| format!("{}/transcode.hang", args.source));

	// Publish the derivative through one origin and consume the source through
	// another, over a single auto-reconnecting session.
	let publish = moq_net::Origin::random().produce();
	let remote = moq_net::Origin::random().produce();

	let client = moq_native::ClientConfig::default().init()?;
	let session = client
		.with_publisher(&publish)
		.with_subscriber(remote.clone())
		.reconnect(args.url.clone());

	// Wait for the source to be announced rather than for the session to connect:
	// `request_broadcast` answers on the spot, so asking the moment a session exists
	// races the announcement that makes the path routable.
	//
	// Raced against the session ending, since the wait itself never fails: the origin
	// outlives the session here, so a rejected token or an exhausted retry budget would
	// otherwise leave us waiting for an announcement that can never arrive.
	let consumer = remote.consume();
	tokio::select! {
		announced = consumer.announced_broadcast(&args.source) => {
			announced.context("origin closed before the source broadcast was announced")?;
		}
		closed = session.closed() => {
			closed.context("session failed before the source broadcast was announced")?;
			anyhow::bail!("session closed before the source broadcast was announced");
		}
	}

	// Resolve it for real; the session subscribes upstream on demand.
	let source = consumer
		.request_broadcast(&args.source)
		.await
		.context("source broadcast unavailable")?;

	let mut config = moq_transcode::Config::default();
	// The derivative lives one level below the source, so the source is `..`.
	// The default ladder and encoder (hardware first: NVENC on Linux) apply.
	config.source = Some(moq_net::PathRelativeOwned::from("..".to_string()));

	let output = publish
		.create_broadcast(&output_path, moq_net::broadcast::Route::new().with_announce(true))
		.context("failed to create the derivative broadcast")?;
	tracing::info!(source = %args.source, output = %output_path, "transcoding");

	tokio::select! {
		res = moq_transcode::run(source, output, config) => Ok(res?),
		res = session.closed() => Ok(res?),
	}
}
