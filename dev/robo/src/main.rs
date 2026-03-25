use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Context;
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

    /// Directory containing media files (idle.fmp4, dead.fmp4, and action *.fmp4 files).
    #[arg(long, default_value = "../media/robo")]
    pub media_dir: PathBuf,

    /// The MoQ client configuration.
    #[command(flatten)]
    pub client: moq_native::ClientConfig,

    /// The log configuration.
    #[command(flatten)]
    pub log: moq_native::Log,
}

/// Discovered media files from the --media-dir directory.
#[derive(Clone)]
pub struct MediaFiles {
    pub idle: PathBuf,
    pub dead: PathBuf,
    /// Action name → file path, sorted alphabetically.
    pub actions: BTreeMap<String, PathBuf>,
}

impl MediaFiles {
    fn discover(dir: &PathBuf) -> anyhow::Result<Self> {
        anyhow::ensure!(dir.is_dir(), "media dir not found: {}", dir.display());

        let idle = dir.join("idle.fmp4");
        anyhow::ensure!(idle.exists(), "missing idle.fmp4 in {}", dir.display());

        let dead = dir.join("dead.fmp4");
        anyhow::ensure!(dead.exists(), "missing dead.fmp4 in {}", dir.display());

        let mut actions = BTreeMap::new();
        for entry in std::fs::read_dir(dir).context("reading media dir")? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("fmp4") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            if stem == "idle" || stem == "dead" {
                continue;
            }
            actions.insert(stem.to_string(), path);
        }

        anyhow::ensure!(
            !actions.is_empty(),
            "no action fmp4 files found in {}",
            dir.display()
        );

        tracing::info!(
            idle = %idle.display(),
            dead = %dead.display(),
            actions = ?actions.keys().collect::<Vec<_>>(),
            "discovered media files"
        );

        Ok(Self {
            idle,
            dead,
            actions,
        })
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::parse();
    config.log.init();

    let media = MediaFiles::discover(&config.media_dir)?;
    let client = config.client.clone().init()?;
    let robo = robo::Robo::new(&config, &media);

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
            std::process::exit(0);
        },
    }
}
