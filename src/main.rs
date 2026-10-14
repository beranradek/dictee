mod audio;
mod codex_app;
mod config;
mod controller;
mod deliver;
mod download;
mod ipc;
mod models;
mod qwen;
mod rewrite;
mod rewrite_api;
mod shortcut;
mod stt;
mod ui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "dictee", version, about = "Local speech-to-text dictation for Linux (Wayland friendly)")]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Transcribe a WAV file and print the text (no daemon, no clipboard)
    Transcribe {
        file: PathBuf,
        /// Also run the configured rewrite engine
        #[arg(long)]
        rewrite: bool,
    },
    /// Run the dictation daemon (default when no command is given)
    Daemon,
    /// Open the settings window (starts the daemon if needed)
    Settings,
    /// Start / stop / toggle / cancel dictation, or print the daemon status
    Start,
    Stop,
    Toggle,
    Cancel,
    Status,
    /// Stop the daemon
    Quit,
    /// List microphones
    Mics,
    /// List models, or download one with --download <id>
    Models {
        #[arg(long)]
        download: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Cmd::Transcribe { file, rewrite }) => {
            let mut cfg = config::Config::load()?;
            if !rewrite {
                cfg.rewrite.engine = "none".into();
            }
            let samples = audio::read_wav(&file)?;
            let mut stt = stt::Stt::default();
            let out = controller::process(&cfg, &mut stt, &samples, |_| {})?;
            if let Some(e) = &out.rewrite_error {
                eprintln!("rewrite failed: {e}");
            }
            if rewrite {
                eprintln!("raw: {}", out.raw);
            }
            println!("{}", out.text);
        }
        Some(Cmd::Mics) => {
            for m in audio::list_mics()? {
                println!("{}\t{}", m.node_name, m.description);
            }
        }
        Some(Cmd::Models { download }) => {
            if let Some(id) = download {
                let mut last = 0;
                let mut report = |d: u64, t: u64| {
                    let pct = if t > 0 { d * 100 / t } else { 0 };
                    if pct != last {
                        last = pct;
                        eprint!("\r{pct}%");
                    }
                };
                if qwen::find(&id).is_ok() {
                    let mut q = config::Config::load()?.qwen;
                    q.model = id;
                    qwen::download_all(&q, &mut report)?;
                } else {
                    models::download(&id, &mut report)?;
                }
                eprintln!("\ndone");
            }
            println!("Whisper (stt_engine = \"whisper\"):");
            for m in models::MODELS {
                println!("  {}\t{}\t{} MB\t{}", m.id, if models::is_downloaded(m.id) { "downloaded" } else { "-" }, m.size_mb, m.label);
            }
            println!("Qwen3-ASR on llama.cpp (stt_engine = \"qwen\"):");
            for m in qwen::MODELS {
                println!("  {}\t{}\t{} MB\t{}", m.id, if qwen::model_ready(m.id) { "downloaded" } else { "-" }, m.total_mb(), m.label);
            }
        }
        None | Some(Cmd::Daemon) => daemon(false)?,
        Some(Cmd::Settings) => {
            if ipc::daemon_running() {
                ipc::send("settings")?;
            } else {
                daemon(true)?;
            }
        }
        Some(c) => {
            let word = match c {
                Cmd::Start => "start",
                Cmd::Stop => "stop",
                Cmd::Toggle => "toggle",
                Cmd::Cancel => "cancel",
                Cmd::Status => "status",
                _ => "quit",
            };
            println!("{}", ipc::send(word)?);
        }
    }
    Ok(())
}

fn daemon(show_settings: bool) -> Result<()> {
    use std::sync::{mpsc, Arc, Mutex};
    config::Config::load()?; // fail early (and write defaults) on first start
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (ev_tx, ev_rx) = mpsc::channel();
    let state = Arc::new(Mutex::new(controller::State::Idle));
    ipc::serve(cmd_tx.clone(), state.clone())?;
    let status: shortcut::Status = Arc::new(Mutex::new("Shortcut: registering...".into()));
    shortcut::spawn(cmd_tx.clone(), status.clone());
    let s2 = state.clone();
    std::thread::spawn(move || controller::run(cmd_rx, ev_tx, s2));
    ui::run(cmd_tx, ev_rx, state, status, show_settings);
    let _ = std::fs::remove_file(ipc::socket_path());
    Ok(())
}
