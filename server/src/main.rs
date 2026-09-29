//! `wisprcheap-server`: run the sync server, create the admin account, back up the database.

use std::io::Read;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use wisprcheap_server::{AppState, Config, Db, app, auth, db, web};

#[derive(Parser)]
#[command(name = "wisprcheap-server", version, about = "Self-hostable sync server for wisprcheap. Configured with WCS_* environment variables.")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server (the default).
    Serve,
    /// Manage accounts.
    Admin {
        #[command(subcommand)]
        command: AdminCommand,
    },
    /// Consistent copy of the database into <data dir>/backups (keeps the last 7).
    Backup {
        /// Target directory (default: <data dir>/backups).
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum AdminCommand {
    /// Create an admin account (asks for the password).
    Create {
        username: String,
        /// Read the password from standard input instead of asking.
        #[arg(long)]
        password_stdin: bool,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("WCS_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();
    let config = Config::from_env()?;
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => serve(config),
        Command::Admin { command: AdminCommand::Create { username, password_stdin } } => create_admin(&config, &username, password_stdin),
        Command::Backup { dir } => backup(&config, dir),
    }
}

fn serve(config: Config) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        let db = Db::open(&config.db_path())?;
        let bind = config.bind;
        let state = AppState::new(config, db);
        if let Some(token) = web::new_setup_token(&state)? {
            tracing::warn!(
                "No account yet. Create the admin account at {}/setup?token={token} (or run `wisprcheap-server admin create <username>`).",
                state.config.public_url
            );
        }
        if std::env::var("WCS_BACKUP_DAILY").is_ok_and(|v| v == "1") {
            let s = state.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(86_400)).await;
                    if let Err(e) = backup_state(&s.db, &s.config.data_dir.join("backups")) {
                        tracing::error!("daily backup failed: {e:#}");
                    }
                }
            });
        }
        let listener = tokio::net::TcpListener::bind(bind).await.with_context(|| format!("binding {bind}"))?;
        tracing::info!("wisprcheap-server {} listening on {bind}, public URL {}", wisprcheap_server::VERSION, state.config.public_url);
        axum::serve(listener, app(state).into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
                tracing::info!("shutting down");
            })
            .await?;
        Ok(())
    })
}

fn create_admin(config: &Config, username: &str, password_stdin: bool) -> Result<()> {
    auth::check_username(username).map_err(anyhow::Error::msg)?;
    let password = if password_stdin {
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text)?;
        text.trim_end_matches(['\r', '\n']).to_string()
    } else {
        let first = rpassword::prompt_password(format!("Password for {username}: "))?;
        let again = rpassword::prompt_password("Again: ")?;
        if first != again {
            bail!("the passwords differ");
        }
        first
    };
    auth::check_password_rules(username, &password).map_err(anyhow::Error::msg)?;
    let database = Db::open(&config.db_path())?;
    let conn = database.lock();
    if db::user_by_name(&conn, username)?.is_some() {
        bail!("user {username} already exists");
    }
    let id = db::create_user(&conn, username, &auth::hash_password(&password)?, true)?;
    db::meta_delete(&conn, "setup_token_hash")?;
    db::audit(&conn, Some(&id), None, "admin_created", Some(username), "cli");
    println!("Admin {username} created. Log in at {}/login", config.public_url);
    Ok(())
}

fn backup_state(database: &Db, dir: &std::path::Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let target = dir.join(format!("wisprcheap-{}.db", chrono::Utc::now().format("%Y%m%d-%H%M%S")));
    database.backup_to(&target)?;
    let mut old: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("wisprcheap-") && n.ends_with(".db")))
        .collect();
    old.sort();
    while old.len() > 7 {
        let _ = std::fs::remove_file(old.remove(0));
    }
    tracing::info!("backup written to {}", target.display());
    Ok(target)
}

fn backup(config: &Config, dir: Option<PathBuf>) -> Result<()> {
    let database = Db::open(&config.db_path())?;
    let target = backup_state(&database, &dir.unwrap_or_else(|| config.data_dir.join("backups")))?;
    println!("{}", target.display());
    Ok(())
}
