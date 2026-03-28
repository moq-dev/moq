use clap::Parser;
use url::Url;

mod drone;
mod game;
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

    /// The drone ID (used in broadcast path: drone/{id}). Random if not provided.
    #[arg(long, default_value_t = random_id())]
    pub id: String,

    /// Number of drone instances to spawn (each gets a unique ID).
    #[arg(long, default_value_t = 1)]
    pub count: usize,

    /// The MoQ client configuration.
    #[command(flatten)]
    pub client: moq_native::ClientConfig,

    /// The log configuration.
    #[command(flatten)]
    pub log: moq_native::Log,
}

async fn run_one(config: &Config, id: String) -> anyhow::Result<()> {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(64);
    let client = config.client.clone().init()?;
    let d = drone::Drone::new(config, cmd_tx);

    // Publish origin: the drone broadcast.
    let publish_origin = moq_lite::Origin::produce();
    let broadcast_path = format!("drone/{id}");
    publish_origin.publish_broadcast(&broadcast_path, d.consume());

    // Consume origin: only viewer broadcasts under drone/{id}/viewer/.
    let viewer_prefix = format!("drone/{id}/viewer");
    let consume_origin = moq_lite::Origin::produce();
    let mut viewer_consumer = consume_origin
        .with_root(&viewer_prefix)
        .expect("viewer prefix should be valid")
        .consume();

    tracing::info!(url = %config.url, %id, "connecting to relay");

    let session = client
        .with_publish(publish_origin.consume())
        .with_consume(consume_origin)
        .connect(config.url.clone())
        .await?;

    tokio::select! {
        res = d.run(cmd_rx) => res,
        res = session.closed() => res.map_err(Into::into),
        res = drone::handle_viewers(&mut viewer_consumer, &d) => res,
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::parse();
    config.log.init();

    if config.count == 1 {
        tokio::select! {
            res = run_one(&config, config.id.clone()) => res,
            _ = tokio::signal::ctrl_c() => std::process::exit(0),
        }
    } else {
        let mut handles = Vec::new();
        for i in 0..config.count {
            let id = format!("{}-{i}", config.id);
            let cfg = config.clone();
            handles.push(tokio::spawn(async move {
                if let Err(e) = run_one(&cfg, id.clone()).await {
                    tracing::error!(%id, error = %e, "drone instance failed");
                }
            }));
        }

        tokio::select! {
            _ = async {
                for h in handles {
                    let _ = h.await;
                }
            } => Ok(()),
            _ = tokio::signal::ctrl_c() => std::process::exit(0),
        }
    }
}
