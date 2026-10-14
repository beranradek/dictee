mod audio;
mod codex_app;
mod config;
mod controller;
mod deliver;
mod download;
mod ipc;
mod models;
mod options;
mod qwen;
mod rewrite;
mod rewrite_api;
mod shortcut;
mod stt;
mod ui;
mod util;

use anyhow::Result;
use clap::{Parser, Subcommand};
use config::Config;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "dictee", version, about = "Local speech-to-text dictation for Linux (Wayland friendly)")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the dictation daemon (default when no command is given)
    Daemon,
    /// Open the settings window (starts the daemon if needed)
    Settings,
    /// Start dictation
    Start,
    /// Stop dictation and deliver the text
    Stop,
    /// Start dictation, or stop it when it is running
    Toggle,
    /// Cancel dictation without text
    Cancel,
    /// Print the daemon state (idle, recording, transcribing, rewriting)
    Status,
    /// Stop the daemon
    Quit,
    /// Transcribe a WAV file and print the text (no daemon, no clipboard)
    Transcribe {
        file: PathBuf,
        /// Also run the configured rewrite engine
        #[arg(long)]
        rewrite: bool,
    },
    /// List microphones
    Mics,
    /// List models, or download one with --download <id>
    Models {
        #[arg(long)]
        download: Option<String>,
    },
}

impl Command {
    /// The word the daemon understands, for the commands that are sent to it.
    fn daemon_word(&self) -> Option<&'static str> {
        Some(match self {
            Command::Start => "start",
            Command::Stop => "stop",
            Command::Toggle => "toggle",
            Command::Cancel => "cancel",
            Command::Status => "status",
            Command::Quit => "quit",
            _ => return None,
        })
    }
}

fn main() -> Result<()> {
    match Cli::parse().command {
        None | Some(Command::Daemon) => daemon(false),
        Some(Command::Settings) => {
            if ipc::daemon_running() {
                ipc::send("settings")?;
                Ok(())
            } else {
                daemon(true)
            }
        }
        Some(Command::Transcribe { file, rewrite }) => transcribe(&file, rewrite),
        Some(Command::Mics) => {
            for m in audio::list_mics()? {
                println!("{}\t{}", m.node_name, m.description);
            }
            Ok(())
        }
        Some(Command::Models { download }) => list_models(download),
        Some(other) => {
            println!("{}", ipc::send(other.daemon_word().expect("daemon command"))?);
            Ok(())
        }
    }
}

fn transcribe(file: &std::path::Path, rewrite: bool) -> Result<()> {
    let mut cfg = Config::load()?;
    if !rewrite {
        cfg.rewrite.engine = "none".into();
    }
    let samples = audio::read_wav(file)?;
    let out = controller::process(&cfg, &mut stt::Stt::default(), &samples, |_| {})?;
    if let Some(e) = &out.rewrite_error {
        eprintln!("rewrite failed: {e}");
    }
    if rewrite {
        eprintln!("raw: {}", out.raw);
    }
    println!("{}", out.text);
    Ok(())
}

fn list_models(download: Option<String>) -> Result<()> {
    let cfg = Config::load()?;
    let root = cfg.models_path();
    if let Some(id) = download {
        let mut last = 0;
        let mut report = |done: u64, total: u64| {
            let pct = (done * 100).checked_div(total).unwrap_or(0);
            if pct != last {
                last = pct;
                eprint!("\r{pct}%");
            }
        };
        if qwen::find(&id).is_ok() {
            let mut q = cfg.qwen.clone();
            q.model = id;
            qwen::download_all(&root, &q, &mut report)?;
        } else {
            models::download(&root, &id, &mut report)?;
        }
        eprintln!("\ndone");
    }
    let state = |ready| if ready { "downloaded" } else { "-" };
    println!("Models folder: {}", root.display());
    println!("Whisper (stt_engine = \"whisper\"):");
    for m in models::MODELS {
        println!("  {}\t{}\t{} MB\t{}", m.id, state(models::is_downloaded(&root, m.id)), m.size_mb(), m.label);
    }
    println!("Qwen3-ASR on llama.cpp (stt_engine = \"qwen\"):");
    for m in qwen::MODELS {
        println!("  {}\t{}\t{} MB\t{}", m.id, state(qwen::model_ready(&root, m.id)), m.total_mb(), m.label);
    }
    Ok(())
}

fn daemon(show_settings: bool) -> Result<()> {
    use std::sync::{Arc, Mutex, mpsc};
    Config::load()?; // fail early (and write defaults) on first start
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (event_tx, event_rx) = async_channel::unbounded();
    let state = Arc::new(Mutex::new(controller::State::Idle));
    ipc::serve(cmd_tx.clone(), state.clone())?;
    let status: shortcut::Status = Arc::new(Mutex::new("Shortcut: registering...".into()));
    if std::env::var_os("DICTEE_NO_PORTAL").is_none() {
        shortcut::spawn(cmd_tx.clone(), status.clone()); // not for tests: they use their own daemon and no shortcut dialogs
    }
    let controller_tx = cmd_tx.clone();
    std::thread::spawn(move || controller::Controller::new(stt::Stt::default(), event_tx, state, controller_tx).run(cmd_rx));
    ui::run(cmd_tx, event_rx, status, show_settings);
    let _ = std::fs::remove_file(ipc::socket_path());
    Ok(())
}
