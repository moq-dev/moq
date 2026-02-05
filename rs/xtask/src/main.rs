//! xtask - Development task runner for MoQ
//!
//! Run with: `cargo xtask <command>`
//!
//! This replaces the justfile with a pure Rust solution.

use std::{
    env, fs,
    io::IsTerminal,
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
};

use clap::{Parser, Subcommand};

/// MoQ development task runner
#[derive(Parser)]
#[command(name = "xtask", about = "MoQ development task runner")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Install dependencies
    Install,

    /// Run all development services (relay, web server, publish bbb)
    Dev,

    /// Run a localhost relay server without authentication
    Relay {
        /// Additional arguments to pass to moq-relay
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },

    /// Run a cluster of relay servers
    Cluster,

    /// Run a localhost root server
    Root,

    /// Run a localhost leaf server
    Leaf,

    /// Authentication commands
    Auth {
        #[command(subcommand)]
        command: AuthCommands,
    },

    /// Download test videos
    Download {
        /// Video name (bbb, tos, av1, hevc)
        name: String,
    },

    /// Publish media to a relay
    Pub {
        #[command(subcommand)]
        command: PubCommands,
    },

    /// Serve media directly (without relay)
    Serve {
        #[command(subcommand)]
        command: ServeCommands,
    },

    /// Run the web development server
    Web {
        /// Relay URL
        #[arg(default_value = "http://localhost:4443/anon")]
        url: String,
    },

    /// Clock broadcast commands
    Clock {
        /// Action: publish or subscribe
        action: String,
        /// Relay URL
        #[arg(default_value = "http://localhost:4443/anon")]
        url: String,
        /// Additional arguments
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },

    /// Run CI checks
    Check {
        #[command(subcommand)]
        command: Option<CheckCommands>,
    },

    /// Run tests
    Test {
        #[command(subcommand)]
        command: Option<TestCommands>,
    },

    /// Auto-fix linting issues
    Fix,

    /// Build all packages
    Build,

    /// Upgrade dependencies
    Update,

    /// Tokio console commands
    Console {
        #[command(subcommand)]
        command: ConsoleCommands,
    },

    /// Serve documentation locally
    Doc,

    /// Throttle UDP traffic for testing (macOS only)
    Throttle,
}

#[derive(Subcommand)]
enum AuthCommands {
    /// Generate a random secret key for authentication
    Key,
    /// Generate authentication tokens for local development
    Token,
}

#[derive(Subcommand)]
enum PubCommands {
    /// Publish using fMP4 format (default)
    Fmp4 {
        /// Video name (bbb, tos, av1, hevc)
        name: String,
        /// Relay URL
        #[arg(default_value = "http://localhost:4443/anon")]
        url: String,
        /// Additional arguments
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Publish using HLS format
    Hls {
        /// Video name (bbb, tos, av1, hevc)
        name: String,
        /// Relay URL
        #[arg(default_value = "http://localhost:4443/anon")]
        relay: String,
    },
    /// Publish using H.264 Annex B format
    H264 {
        /// Video name (bbb, tos, av1, hevc)
        name: String,
        /// Relay URL
        #[arg(default_value = "http://localhost:4443/anon")]
        url: String,
        /// Additional arguments
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Publish using Iroh transport
    Iroh {
        /// Video name (bbb, tos, av1, hevc)
        name: String,
        /// Relay URL
        url: String,
        /// Prefix for the broadcast name
        #[arg(default_value = "")]
        prefix: String,
    },
    /// Publish using GStreamer (deprecated)
    Gst {
        /// Video name
        name: String,
        /// Relay URL
        #[arg(default_value = "http://localhost:4443/anon")]
        url: String,
    },
}

#[derive(Subcommand)]
enum ServeCommands {
    /// Serve using fMP4 format
    Fmp4 {
        /// Video name (bbb, tos, av1, hevc)
        name: String,
        /// Additional arguments (e.g., --iroh-enabled)
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Generate and serve an HLS stream for testing
    Hls {
        /// Video name (bbb, tos, av1, hevc)
        name: String,
        /// HTTP server port
        #[arg(default_value = "8000")]
        port: String,
    },
}

#[derive(Subcommand)]
enum CheckCommands {
    /// Run comprehensive checks including all feature combinations
    All,
}

#[derive(Subcommand)]
enum TestCommands {
    /// Run comprehensive tests including all feature combinations
    All,
}

#[derive(Subcommand)]
enum ConsoleCommands {
    /// Connect to the relay server (port 6680)
    Relay,
    /// Connect to the publisher (port 6681)
    Pub,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Install => install(),
        Commands::Dev => dev(),
        Commands::Relay { args } => relay(&args),
        Commands::Cluster => cluster(),
        Commands::Root => root(),
        Commands::Leaf => leaf(),
        Commands::Auth { command } => match command {
            AuthCommands::Key => auth_key(),
            AuthCommands::Token => auth_token(),
        },
        Commands::Download { name } => download(&name),
        Commands::Pub { command } => match command {
            PubCommands::Fmp4 { name, url, args } => pub_fmp4(&name, &url, &args),
            PubCommands::Hls { name, relay } => pub_hls(&name, &relay),
            PubCommands::H264 { name, url, args } => pub_h264(&name, &url, &args),
            PubCommands::Iroh { name, url, prefix } => pub_iroh(&name, &url, &prefix),
            PubCommands::Gst { name: _, url: _ } => pub_gst(),
        },
        Commands::Serve { command } => match command {
            ServeCommands::Fmp4 { name, args } => serve_fmp4(&name, &args),
            ServeCommands::Hls { name, port } => serve_hls(&name, &port),
        },
        Commands::Web { url } => web(&url),
        Commands::Clock { action, url, args } => clock(&action, &url, &args),
        Commands::Check { command } => match command {
            Some(CheckCommands::All) => check_all(),
            None => check(),
        },
        Commands::Test { command } => match command {
            Some(TestCommands::All) => test_all(),
            None => test(),
        },
        Commands::Fix => fix(),
        Commands::Build => build(),
        Commands::Update => update(),
        Commands::Console { command } => match command {
            ConsoleCommands::Relay => console_relay(),
            ConsoleCommands::Pub => console_pub(),
        },
        Commands::Doc => doc(),
        Commands::Throttle => throttle(),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::FAILURE
        }
    }
}

// Helper functions

fn project_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask should be in rs/xtask")
        .parent()
        .expect("rs should have a parent")
        .to_path_buf()
}

fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let status = Command::new(program)
        .args(args)
        .current_dir(project_root())
        .status()
        .map_err(|e| format!("Failed to run {program}: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} failed with status: {status}"))
    }
}

fn run_with_env(program: &str, args: &[&str], env: &[(&str, &str)]) -> Result<(), String> {
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(project_root());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let status = cmd
        .status()
        .map_err(|e| format!("Failed to run {program}: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} failed with status: {status}"))
    }
}

fn run_in_dir(program: &str, args: &[&str], dir: &Path) -> Result<(), String> {
    let status = Command::new(program)
        .args(args)
        .current_dir(dir)
        .status()
        .map_err(|e| format!("Failed to run {program}: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} failed with status: {status}"))
    }
}

fn cargo() -> String {
    env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

fn is_tty() -> bool {
    std::io::stdout().is_terminal()
}

fn file_exists(path: &Path) -> bool {
    path.exists()
}

fn command_exists(cmd: &str) -> bool {
    Command::new("which")
        .arg(cmd)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// Command implementations

fn install() -> Result<(), String> {
    run("bun", &["install"])?;
    run(
        &cargo(),
        &[
            "install",
            "--locked",
            "cargo-shear",
            "cargo-sort",
            "cargo-upgrades",
            "cargo-edit",
            "cargo-hack",
        ],
    )
}

fn dev() -> Result<(), String> {
    run("bun", &["install"])?;
    run(&cargo(), &["build"])?;
    run(
        "bun",
        &[
            "run",
            "concurrently",
            "--kill-others",
            "--names",
            "srv,bbb,web",
            "--prefix-colors",
            "auto",
            "cargo xtask relay",
            "sleep 1 && cargo xtask pub fmp4 bbb http://localhost:4443/anon",
            "sleep 2 && cargo xtask web http://localhost:4443/anon",
        ],
    )
}

fn relay(args: &[String]) -> Result<(), String> {
    let mut cmd_args = vec![
        "run",
        "--bin",
        "moq-relay",
        "--",
        "dev/relay.toml",
    ];
    let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    cmd_args.extend(args_refs);

    run_with_env(
        &cargo(),
        &cmd_args,
        &[("TOKIO_CONSOLE_BIND", "127.0.0.1:6680")],
    )
}

fn cluster() -> Result<(), String> {
    run("bun", &["install"])?;
    auth_token()?;
    run(&cargo(), &["build", "--bin", "moq-relay"])?;

    // Read JWT tokens
    let root = project_root();
    let demo_cli_jwt = fs::read_to_string(root.join("dev/demo-cli.jwt"))
        .map_err(|e| format!("Failed to read demo-cli.jwt: {e}"))?
        .trim()
        .to_string();
    let demo_web_jwt = fs::read_to_string(root.join("dev/demo-web.jwt"))
        .map_err(|e| format!("Failed to read demo-web.jwt: {e}"))?
        .trim()
        .to_string();

    run(
        "bun",
        &[
            "run",
            "concurrently",
            "--kill-others",
            "--names",
            "root,leaf,bbb,tos,web",
            "--prefix-colors",
            "auto",
            "cargo xtask root",
            "sleep 1 && cargo xtask leaf",
            &format!(
                "sleep 2 && cargo xtask pub fmp4 bbb 'http://localhost:4444/demo?jwt={demo_cli_jwt}'"
            ),
            &format!(
                "sleep 3 && cargo xtask pub fmp4 tos 'http://localhost:4443/demo?jwt={demo_cli_jwt}'"
            ),
            &format!(
                "sleep 4 && cargo xtask web 'http://localhost:4443/demo?jwt={demo_web_jwt}'"
            ),
        ],
    )
}

fn root() -> Result<(), String> {
    auth_key()?;
    run(&cargo(), &["run", "--bin", "moq-relay", "--", "dev/root.toml"])
}

fn leaf() -> Result<(), String> {
    auth_token()?;
    run(&cargo(), &["run", "--bin", "moq-relay", "--", "dev/leaf.toml"])
}

fn auth_key() -> Result<(), String> {
    let root = project_root();
    let key_path = root.join("dev/root.jwk");

    if !file_exists(&key_path) {
        // Remove any existing JWT files
        for entry in fs::read_dir(root.join("dev")).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.path().extension().map(|e| e == "jwt").unwrap_or(false) {
                fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
            }
        }
        run(
            &cargo(),
            &["run", "--bin", "moq-token", "--", "--key", "dev/root.jwk", "generate"],
        )?;
    }
    Ok(())
}

fn auth_token() -> Result<(), String> {
    auth_key()?;

    let root = project_root();

    // Generate demo-web.jwt
    if !file_exists(&root.join("dev/demo-web.jwt")) {
        let output = Command::new(cargo())
            .args([
                "run",
                "--quiet",
                "--bin",
                "moq-token",
                "--",
                "--key",
                "dev/root.jwk",
                "sign",
                "--root",
                "demo",
                "--subscribe",
                "",
                "--publish",
                "me",
            ])
            .current_dir(&root)
            .output()
            .map_err(|e| format!("Failed to run moq-token: {e}"))?;

        if !output.status.success() {
            return Err(format!(
                "moq-token failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        fs::write(root.join("dev/demo-web.jwt"), output.stdout)
            .map_err(|e| format!("Failed to write demo-web.jwt: {e}"))?;
    }

    // Generate demo-cli.jwt
    if !file_exists(&root.join("dev/demo-cli.jwt")) {
        let output = Command::new(cargo())
            .args([
                "run",
                "--quiet",
                "--bin",
                "moq-token",
                "--",
                "--key",
                "dev/root.jwk",
                "sign",
                "--root",
                "demo",
                "--publish",
                "",
            ])
            .current_dir(&root)
            .output()
            .map_err(|e| format!("Failed to run moq-token: {e}"))?;

        if !output.status.success() {
            return Err(format!(
                "moq-token failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        fs::write(root.join("dev/demo-cli.jwt"), output.stdout)
            .map_err(|e| format!("Failed to write demo-cli.jwt: {e}"))?;
    }

    // Generate root.jwt
    if !file_exists(&root.join("dev/root.jwt")) {
        let output = Command::new(cargo())
            .args([
                "run",
                "--quiet",
                "--bin",
                "moq-token",
                "--",
                "--key",
                "dev/root.jwk",
                "sign",
                "--root",
                "",
                "--subscribe",
                "",
                "--publish",
                "",
                "--cluster",
            ])
            .current_dir(&root)
            .output()
            .map_err(|e| format!("Failed to run moq-token: {e}"))?;

        if !output.status.success() {
            return Err(format!(
                "moq-token failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        fs::write(root.join("dev/root.jwt"), output.stdout)
            .map_err(|e| format!("Failed to write root.jwt: {e}"))?;
    }

    Ok(())
}

fn download_url(name: &str) -> Result<&'static str, String> {
    match name {
        "bbb" => Ok("http://commondatastorage.googleapis.com/gtv-videos-bucket/sample/BigBuckBunny.mp4"),
        "tos" => Ok("http://commondatastorage.googleapis.com/gtv-videos-bucket/sample/TearsOfSteel.mp4"),
        "av1" => Ok("http://download.opencontent.netflix.com.s3.amazonaws.com/AV1/Sparks/Sparks-5994fps-AV1-10bit-1920x1080-2194kbps.mp4"),
        "hevc" => Ok("https://test-videos.co.uk/vids/jellyfish/mp4/h265/1080/Jellyfish_1080_10s_30MB.mp4"),
        _ => Err(format!("Unknown video name: {name}. Use: bbb, tos, av1, hevc")),
    }
}

fn download(name: &str) -> Result<(), String> {
    let root = project_root();
    let mp4_path = root.join(format!("dev/{name}.mp4"));
    let fmp4_path = root.join(format!("dev/{name}.fmp4"));

    // Download if not exists
    if !file_exists(&mp4_path) {
        let url = download_url(name)?;
        println!("Downloading {name}.mp4...");
        run("curl", &["-fsSL", url, "-o", mp4_path.to_str().unwrap()])?;
    }

    // Convert to fmp4 if not exists
    if !file_exists(&fmp4_path) {
        println!("Converting to fragmented MP4...");
        run(
            "ffmpeg",
            &[
                "-loglevel",
                "error",
                "-i",
                mp4_path.to_str().unwrap(),
                "-c:v",
                "copy",
                "-f",
                "mp4",
                "-movflags",
                "cmaf+separate_moof+delay_moov+skip_trailer+frag_every_frame",
                fmp4_path.to_str().unwrap(),
            ],
        )?;
    }

    Ok(())
}

fn ffmpeg_cmaf(input: &str) -> Command {
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-hide_banner",
        "-v",
        "quiet",
        "-stream_loop",
        "-1",
        "-re",
        "-i",
        input,
        "-c",
        "copy",
        "-f",
        "mp4",
        "-movflags",
        "cmaf+separate_moof+delay_moov+skip_trailer+frag_every_frame",
        "-",
    ])
    .current_dir(project_root())
    .stdout(Stdio::piped());
    cmd
}

fn pub_fmp4(name: &str, url: &str, args: &[String]) -> Result<(), String> {
    download(name)?;
    run(&cargo(), &["build", "--bin", "moq"])?;

    let root = project_root();
    let input = root.join(format!("dev/{name}.fmp4"));

    let mut ffmpeg = ffmpeg_cmaf(input.to_str().unwrap())
        .spawn()
        .map_err(|e| format!("Failed to start ffmpeg: {e}"))?;

    let ffmpeg_stdout = ffmpeg.stdout.take().unwrap();

    let mut moq_args = vec![
        "run".to_string(),
        "--bin".to_string(),
        "moq".to_string(),
        "--".to_string(),
    ];
    moq_args.extend(args.iter().cloned());
    moq_args.extend([
        "publish".to_string(),
        "--url".to_string(),
        url.to_string(),
        "--name".to_string(),
        name.to_string(),
        "fmp4".to_string(),
    ]);

    let status = Command::new(cargo())
        .args(&moq_args)
        .current_dir(&root)
        .stdin(ffmpeg_stdout)
        .status()
        .map_err(|e| format!("Failed to run moq: {e}"))?;

    ffmpeg.wait().ok();

    if status.success() {
        Ok(())
    } else {
        Err(format!("moq failed with status: {status}"))
    }
}

fn pub_hls(name: &str, relay: &str) -> Result<(), String> {
    download(name)?;

    let root = project_root();
    let input = root.join(format!("dev/{name}.mp4"));
    let out_dir = root.join(format!("dev/{name}"));

    // Clean and create output directory
    if out_dir.exists() {
        fs::remove_dir_all(&out_dir).map_err(|e| e.to_string())?;
    }
    fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;

    println!(">>> Generating HLS stream to disk (1280x720 + 256x144)...");

    // Start ffmpeg in the background
    let mut ffmpeg = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "warning",
            "-re",
            "-stream_loop",
            "-1",
            "-i",
            input.to_str().unwrap(),
            "-filter_complex",
            "[0:v]split=2[v0][v1];[v0]scale=-2:720[v720];[v1]scale=-2:144[v144]",
            "-map",
            "[v720]",
            "-map",
            "[v144]",
            "-map",
            "0:a:0",
            "-r",
            "25",
            "-preset",
            "veryfast",
            "-g",
            "50",
            "-keyint_min",
            "50",
            "-sc_threshold",
            "0",
            "-c:v:0",
            "libx264",
            "-profile:v:0",
            "high",
            "-level:v:0",
            "4.1",
            "-pix_fmt:v:0",
            "yuv420p",
            "-tag:v:0",
            "avc1",
            "-b:v:0",
            "4M",
            "-maxrate:v:0",
            "4.4M",
            "-bufsize:v:0",
            "8M",
            "-c:v:1",
            "libx264",
            "-profile:v:1",
            "high",
            "-level:v:1",
            "4.1",
            "-pix_fmt:v:1",
            "yuv420p",
            "-tag:v:1",
            "avc1",
            "-b:v:1",
            "300k",
            "-maxrate:v:1",
            "330k",
            "-bufsize:v:1",
            "600k",
            "-c:a",
            "aac",
            "-b:a",
            "128k",
            "-f",
            "hls",
            "-hls_time",
            "2",
            "-hls_list_size",
            "6",
            "-hls_flags",
            "independent_segments+delete_segments",
            "-hls_segment_type",
            "fmp4",
            "-master_pl_name",
            "master.m3u8",
            "-var_stream_map",
            "v:0,agroup:audio,name:720 v:1,agroup:audio,name:144 a:0,agroup:audio,name:audio",
            "-hls_segment_filename",
            &format!("{}/v%v/segment_%09d.m4s", out_dir.display()),
            &format!("{}/v%v/stream.m3u8", out_dir.display()),
        ])
        .spawn()
        .map_err(|e| format!("Failed to start ffmpeg: {e}"))?;

    // Wait for master playlist
    println!(">>> Waiting for HLS playlist generation...");
    let master_path = out_dir.join("master.m3u8");
    for _ in 0..60 {
        if file_exists(&master_path) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    if !file_exists(&master_path) {
        ffmpeg.kill().ok();
        return Err("master.m3u8 not generated in time".to_string());
    }

    // Wait for variant playlists
    println!(">>> Waiting for variant playlists...");
    std::thread::sleep(std::time::Duration::from_secs(2));

    // Run moq to ingest from local files
    println!(">>> Running with --passthrough flag");
    let moq_result = run(
        &cargo(),
        &[
            "run",
            "--bin",
            "moq",
            "--",
            "publish",
            "--url",
            relay,
            "--name",
            name,
            "hls",
            "--playlist",
            &format!("{}/master.m3u8", out_dir.display()),
            "--passthrough",
        ],
    );

    ffmpeg.kill().ok();
    ffmpeg.wait().ok();

    moq_result
}

fn pub_h264(name: &str, url: &str, args: &[String]) -> Result<(), String> {
    download(name)?;
    run(&cargo(), &["build", "--bin", "moq"])?;

    let root = project_root();
    let input = root.join(format!("dev/{name}.fmp4"));

    let mut ffmpeg = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-v",
            "quiet",
            "-stream_loop",
            "-1",
            "-re",
            "-i",
            input.to_str().unwrap(),
            "-c:v",
            "copy",
            "-an",
            "-bsf:v",
            "h264_mp4toannexb",
            "-f",
            "h264",
            "-",
        ])
        .current_dir(&root)
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start ffmpeg: {e}"))?;

    let ffmpeg_stdout = ffmpeg.stdout.take().unwrap();

    let mut moq_args = vec![
        "run".to_string(),
        "--bin".to_string(),
        "moq".to_string(),
        "--".to_string(),
    ];
    moq_args.extend(args.iter().cloned());
    moq_args.extend([
        "publish".to_string(),
        "--url".to_string(),
        url.to_string(),
        "--name".to_string(),
        name.to_string(),
        "--format".to_string(),
        "annex-b".to_string(),
    ]);

    let status = Command::new(cargo())
        .args(&moq_args)
        .current_dir(&root)
        .stdin(ffmpeg_stdout)
        .status()
        .map_err(|e| format!("Failed to run moq: {e}"))?;

    ffmpeg.wait().ok();

    if status.success() {
        Ok(())
    } else {
        Err(format!("moq failed with status: {status}"))
    }
}

fn pub_iroh(name: &str, url: &str, prefix: &str) -> Result<(), String> {
    download(name)?;
    run(&cargo(), &["build", "--bin", "moq"])?;

    let root = project_root();
    let input = root.join(format!("dev/{name}.fmp4"));
    let broadcast_name = format!("{prefix}{name}");

    let mut ffmpeg = ffmpeg_cmaf(input.to_str().unwrap())
        .spawn()
        .map_err(|e| format!("Failed to start ffmpeg: {e}"))?;

    let ffmpeg_stdout = ffmpeg.stdout.take().unwrap();

    let status = Command::new(cargo())
        .args([
            "run",
            "--bin",
            "moq",
            "--",
            "--iroh-enabled",
            "publish",
            "--url",
            url,
            "--name",
            &broadcast_name,
            "fmp4",
        ])
        .current_dir(&root)
        .stdin(ffmpeg_stdout)
        .status()
        .map_err(|e| format!("Failed to run moq: {e}"))?;

    ffmpeg.wait().ok();

    if status.success() {
        Ok(())
    } else {
        Err(format!("moq failed with status: {status}"))
    }
}

fn pub_gst() -> Result<(), String> {
    println!("GStreamer plugin has moved to: https://github.com/moq-dev/gstreamer");
    println!("Install and use hang-gst directly for GStreamer functionality");
    Ok(())
}

fn serve_fmp4(name: &str, args: &[String]) -> Result<(), String> {
    download(name)?;
    run(&cargo(), &["build", "--bin", "moq"])?;

    let root = project_root();
    let input = root.join(format!("dev/{name}.fmp4"));

    let mut ffmpeg = ffmpeg_cmaf(input.to_str().unwrap())
        .spawn()
        .map_err(|e| format!("Failed to start ffmpeg: {e}"))?;

    let ffmpeg_stdout = ffmpeg.stdout.take().unwrap();

    let mut moq_args = vec![
        "run".to_string(),
        "--bin".to_string(),
        "moq".to_string(),
        "--".to_string(),
    ];
    moq_args.extend(args.iter().cloned());
    moq_args.extend([
        "serve".to_string(),
        "--listen".to_string(),
        "[::]:4443".to_string(),
        "--tls-generate".to_string(),
        "localhost".to_string(),
        "--name".to_string(),
        name.to_string(),
        "fmp4".to_string(),
    ]);

    let status = Command::new(cargo())
        .args(&moq_args)
        .current_dir(&root)
        .stdin(ffmpeg_stdout)
        .status()
        .map_err(|e| format!("Failed to run moq: {e}"))?;

    ffmpeg.wait().ok();

    if status.success() {
        Ok(())
    } else {
        Err(format!("moq failed with status: {status}"))
    }
}

fn serve_hls(name: &str, port: &str) -> Result<(), String> {
    download(name)?;

    let root = project_root();
    let input = root.join(format!("dev/{name}.mp4"));
    let out_dir = root.join(format!("dev/{name}"));

    // Clean and create output directory
    if out_dir.exists() {
        fs::remove_dir_all(&out_dir).map_err(|e| e.to_string())?;
    }
    fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;

    println!(">>> Starting HLS stream generation...");
    println!(">>> Master playlist: http://localhost:{port}/master.m3u8");

    // Start ffmpeg
    let mut ffmpeg = Command::new("ffmpeg")
        .args([
            "-loglevel",
            "warning",
            "-re",
            "-stream_loop",
            "-1",
            "-i",
            input.to_str().unwrap(),
            "-map",
            "0:v:0",
            "-map",
            "0:v:0",
            "-map",
            "0:a:0",
            "-r",
            "25",
            "-preset",
            "veryfast",
            "-g",
            "50",
            "-keyint_min",
            "50",
            "-sc_threshold",
            "0",
            "-c:v:0",
            "libx264",
            "-profile:v:0",
            "high",
            "-level:v:0",
            "4.1",
            "-pix_fmt:v:0",
            "yuv420p",
            "-tag:v:0",
            "avc1",
            "-bsf:v:0",
            "dump_extra",
            "-b:v:0",
            "4M",
            "-vf:0",
            "scale=1920:-2",
            "-c:v:1",
            "libx264",
            "-profile:v:1",
            "high",
            "-level:v:1",
            "4.1",
            "-pix_fmt:v:1",
            "yuv420p",
            "-tag:v:1",
            "avc1",
            "-bsf:v:1",
            "dump_extra",
            "-b:v:1",
            "300k",
            "-vf:1",
            "scale=256:-2",
            "-c:a",
            "aac",
            "-b:a",
            "128k",
            "-f",
            "hls",
            "-hls_time",
            "2",
            "-hls_list_size",
            "12",
            "-hls_flags",
            "independent_segments+delete_segments",
            "-hls_segment_type",
            "fmp4",
            "-master_pl_name",
            "master.m3u8",
            "-var_stream_map",
            "v:0,agroup:audio v:1,agroup:audio a:0,agroup:audio",
            "-hls_segment_filename",
            &format!("{}/v%v/segment_%09d.m4s", out_dir.display()),
            &format!("{}/v%v/stream.m3u8", out_dir.display()),
        ])
        .spawn()
        .map_err(|e| format!("Failed to start ffmpeg: {e}"))?;

    std::thread::sleep(std::time::Duration::from_secs(2));
    println!(">>> HTTP server: http://localhost:{port}/");

    let http_result = run_in_dir(
        "python3",
        &["-m", "http.server", port],
        &out_dir,
    );

    ffmpeg.kill().ok();
    ffmpeg.wait().ok();

    http_result
}

fn web(url: &str) -> Result<(), String> {
    let root = project_root();
    let demo_dir = root.join("js/hang-demo");

    let mut cmd = Command::new("bun");
    cmd.args(["run", "dev"])
        .current_dir(&demo_dir)
        .env("VITE_RELAY_URL", url);

    let status = cmd.status().map_err(|e| format!("Failed to run bun: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("bun failed with status: {status}"))
    }
}

fn clock(action: &str, url: &str, args: &[String]) -> Result<(), String> {
    if action != "publish" && action != "subscribe" {
        return Err(format!(
            "action must be 'publish' or 'subscribe', got '{action}'"
        ));
    }

    let mut cmd_args = vec![
        "run",
        "--bin",
        "moq-clock",
        "--",
        "--url",
        url,
        "--broadcast",
        "clock",
    ];
    let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    cmd_args.extend(args_refs);
    cmd_args.push(action);

    run(&cargo(), &cmd_args)
}

fn check() -> Result<(), String> {
    // JS checks
    run("bun", &["install", "--frozen-lockfile"])?;
    if is_tty() {
        run("bun", &["run", "--filter=*", "--elide-lines=0", "check"])?;
    } else {
        run("bun", &["run", "--filter=*", "check"])?;
    }
    run("bun", &["biome", "check"])?;

    // Rust checks
    run(&cargo(), &["check", "--all-targets", "--all-features"])?;
    run(
        &cargo(),
        &[
            "clippy",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ],
    )?;
    run(&cargo(), &["fmt", "--all", "--check"])?;

    // Documentation warnings
    run_with_env(
        &cargo(),
        &["doc", "--no-deps", "--workspace"],
        &[("RUSTDOCFLAGS", "-D warnings")],
    )?;

    // cargo-shear
    run(&cargo(), &["shear"])?;

    // cargo-sort
    run(&cargo(), &["sort", "--workspace", "--check"])?;

    // tofu checks (if available)
    if command_exists("tofu") {
        let root = project_root();
        run_in_dir("tofu", &["fmt", "-check", "-recursive"], &root.join("cdn"))?;
    }

    // nix checks (if available)
    if command_exists("nix") {
        run("nix", &["flake", "check"])?;
    }

    Ok(())
}

fn check_all() -> Result<(), String> {
    check()?;

    println!("Checking all feature combinations for hang...");
    run(
        &cargo(),
        &[
            "hack",
            "check",
            "--package",
            "hang",
            "--each-feature",
            "--no-dev-deps",
        ],
    )
}

fn test() -> Result<(), String> {
    // JS tests
    run("bun", &["install", "--frozen-lockfile"])?;
    if is_tty() {
        run("bun", &["run", "--filter=*", "--elide-lines=0", "test"])?;
    } else {
        run("bun", &["run", "--filter=*", "test"])?;
    }

    // Rust tests
    run(&cargo(), &["test", "--all-targets", "--all-features"])
}

fn test_all() -> Result<(), String> {
    test()?;

    println!("Testing all feature combinations for hang...");
    run(
        &cargo(),
        &["hack", "test", "--package", "hang", "--each-feature"],
    )
}

fn fix() -> Result<(), String> {
    // JS fixes
    run("bun", &["install"])?;
    run("bun", &["biome", "check", "--write"])?;

    // Rust fixes
    run(
        &cargo(),
        &[
            "clippy",
            "--fix",
            "--allow-staged",
            "--allow-dirty",
            "--all-targets",
            "--all-features",
        ],
    )?;
    run(&cargo(), &["fmt", "--all"])?;

    // cargo-shear
    run(&cargo(), &["shear", "--fix"])?;

    // cargo-sort
    run(&cargo(), &["sort", "--workspace"])?;

    // tofu fixes (if available)
    if command_exists("tofu") {
        let root = project_root();
        run_in_dir("tofu", &["fmt", "-recursive"], &root.join("cdn"))?;
    }

    Ok(())
}

fn build() -> Result<(), String> {
    run("bun", &["run", "--filter=*", "build"])?;
    run(&cargo(), &["build"])
}

fn update() -> Result<(), String> {
    run("bun", &["update"])?;
    run("bun", &["outdated"])?;

    // Update patch versions
    run(&cargo(), &["update"])?;

    // Update incompatible versions
    run(&cargo(), &["upgrade", "--incompatible"])?;

    // Update nix flake
    run("nix", &["flake", "update"])
}

fn console_relay() -> Result<(), String> {
    run("tokio-console", &["http://127.0.0.1:6680"])
}

fn console_pub() -> Result<(), String> {
    run("tokio-console", &["http://127.0.0.1:6681"])
}

fn doc() -> Result<(), String> {
    let root = project_root();
    run_in_dir("bun", &["run", "dev"], &root.join("doc"))
}

fn throttle() -> Result<(), String> {
    run("dev/throttle", &[])
}
