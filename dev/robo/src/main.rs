use std::path::PathBuf;

use clap::Parser;
use url::Url;

mod robo;
mod sensor;
mod video;

fn random_id() -> String {
    use rand::Rng;
    let bytes: [u8; 4] = rand::rng().random();
    hex::encode(bytes)
}

#[derive(Parser, Clone)]
pub struct Config {
    /// Connect to the given relay URL.
    #[arg(long)]
    pub url: Url,

    /// The robo ID (used in broadcast path: robo/{id}). Random if not provided.
    #[arg(long, default_value_t = random_id())]
    pub id: String,

    /// Video file paths for each camera angle.
    #[arg(long, required = true, num_args = 1..)]
    pub angles: Vec<PathBuf>,

    /// The MoQ client configuration.
    #[command(flatten)]
    pub client: moq_native::ClientConfig,

    /// The log configuration.
    #[command(flatten)]
    pub log: moq_native::Log,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::parse();
    config.log.init();

    // Validate angle files exist before connecting.
    for path in &config.angles {
        anyhow::ensure!(path.exists(), "angle file not found: {}", path.display());
    }

    let client = config.client.clone().init()?;
    let robo = robo::Robo::new(&config);

    // Publish origin: the robo broadcast.
    let publish_origin = moq_lite::Origin::produce();
    let broadcast_path = format!("robo/{}", config.id);
    publish_origin.publish_broadcast(&broadcast_path, robo.consume());

    // Consume origin: only viewer broadcasts under robo/{id}/viewer/.
    // with_root strips the prefix so announced paths are just the viewer ID.
    let viewer_prefix = format!("robo/{}/viewer", config.id);
    let consume_origin = moq_lite::Origin::produce();
    let mut viewer_consumer = consume_origin
        .with_root(&viewer_prefix)
        .expect("viewer prefix should be valid")
        .consume();

    tracing::info!(url = %config.url, id = %config.id, "connecting to relay");

    let session = client
        .with_publish(publish_origin.consume())
        .with_consume(consume_origin)
        .connect(config.url.clone())
        .await?;

    tokio::select! {
        res = robo.run() => res,
        res = session.closed() => res.map_err(Into::into),
        res = robo::handle_viewers(&mut viewer_consumer, &robo) => res,
        _ = tokio::signal::ctrl_c() => {
            // Exit immediately — spawn_blocking threads can't be cancelled gracefully.
            std::process::exit(0);
        },
    }
}
