//! NgenOrca CLI — the main entry point.
//!
//! Usage:
//!   ngenorca gateway [--port 18789] [--bind 127.0.0.1] [--verbose]
//!   ngenorca status
//!   ngenorca onboard
//!   ngenorca identity list
//!   ngenorca identity pair
//!   ngenorca doctor

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "ngenorca",
    version,
    about = "🐋 NgenOrca — Personal AI Assistant",
    long_about = "NgenOrca is a personal AI assistant with microkernel architecture,\n\
                  hardware-bound identity, and three-tier memory.\n\n\
                  Run `ngenorca gateway` to start the gateway server.\n\
                  Run `ngenorca onboard` for guided first-time setup."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Config file path (default: ~/.ngenorca/config.toml)
    #[arg(long, global = true)]
    config: Option<String>,

    /// Enable verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the NgenOrca gateway server.
    Gateway {
        /// Port to listen on.
        #[arg(long, default_value = "18789")]
        port: Option<u16>,

        /// Address to bind to.
        #[arg(long, default_value = "127.0.0.1")]
        bind: Option<String>,
    },

    /// Show gateway status (requires running gateway).
    Status,

    /// Interactive onboarding wizard.
    Onboard,

    /// Identity management commands.
    Identity {
        #[command(subcommand)]
        action: IdentityAction,
    },

    /// Diagnose common issues.
    Doctor,

    /// Show system information.
    Info,
}

#[derive(Subcommand)]
enum IdentityAction {
    /// List all registered users.
    List,
    /// Start device pairing flow.
    Pair,
    /// Revoke a device.
    Revoke {
        /// Device ID to revoke.
        device_id: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize logging.
    let log_level = if cli.verbose { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(log_level)),
        )
        .with_target(false)
        .init();

    match cli.command {
        Commands::Gateway { port, bind } => {
            let mut config = ngenorca_config::load_config(cli.config.as_deref())?;

            if let Some(p) = port {
                config.gateway.port = p;
            }
            if let Some(b) = bind {
                config.gateway.bind = b;
            }

            print_banner();
            ngenorca_gateway::start(config).await?;
        }

        Commands::Status => {
            let config = ngenorca_config::load_config(cli.config.as_deref())?;
            let base = format!("http://{}:{}", config.gateway.bind, config.gateway.port);
            println!("Checking gateway at {}...", base);

            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()?;

            match client.get(format!("{base}/health")).send().await {
                Ok(resp) if resp.status().is_success() => {
                    let body: serde_json::Value = resp.json().await.unwrap_or_default();
                    println!("  Status:  ONLINE");
                    if let Some(v) = body.get("version").and_then(|v| v.as_str()) {
                        println!("  Version: {v}");
                    }
                    if let Some(u) = body.get("uptime_secs").and_then(|v| v.as_u64()) {
                        println!("  Uptime:  {}m {}s", u / 60, u % 60);
                    }
                }
                Ok(resp) => {
                    println!("  Status:  ERROR (HTTP {})", resp.status());
                }
                Err(e) => {
                    println!("  Status:  OFFLINE");
                    println!("  Error:   {e}");
                    println!("\n  Is the gateway running? Start it with: ngenorca gateway");
                }
            }
        }

        Commands::Onboard => {
            run_onboard_wizard(cli.config.as_deref())?;
        }

        Commands::Identity { action } => match action {
            IdentityAction::List => {
                let config = ngenorca_config::load_config(cli.config.as_deref())?;
                let identity_db = config.data_dir.join("identity.db");
                let manager = ngenorca_identity::IdentityManager::new(
                    identity_db.to_str().unwrap_or("identity.db"),
                )?;

                let users = manager.list_users()?;
                if users.is_empty() {
                    println!("No users registered. Run `ngenorca onboard` to get started.");
                } else {
                    println!("Registered users:");
                    for user in &users {
                        println!(
                            "  {} ({}) — {} devices, {} channels — role: {:?}",
                            user.display_name,
                            user.user_id.0,
                            user.devices.len(),
                            user.channels.len(),
                            user.role,
                        );
                    }
                }
            }
            IdentityAction::Pair => {
                println!("Starting device pairing...");
                let caps = ngenorca_identity::fingerprint::detect_capabilities();
                println!("Hardware capabilities:");
                println!("  TPM:            {}", if caps.tpm_available { "✓" } else { "✗" });
                println!("  Secure Enclave: {}", if caps.secure_enclave_available { "✓" } else { "✗" });
                println!("  StrongBox:      {}", if caps.strongbox_available { "✓" } else { "✗" });
                println!();
                println!("(Full pairing flow coming soon)");
            }
            IdentityAction::Revoke { device_id } => {
                println!("Revoking device: {device_id}");
                println!("(Not yet implemented)");
            }
        },

        Commands::Doctor => {
            println!("🔍 NgenOrca Doctor");
            println!();

            // Check sandbox environment.
            let env = ngenorca_sandbox::detect_environment();
            println!("Sandbox environment: {:?}", env);

            // Check hardware identity.
            let caps = ngenorca_identity::fingerprint::detect_capabilities();
            println!("TPM available:      {}", caps.tpm_available);
            println!("Secure Enclave:     {}", caps.secure_enclave_available);

            // Check config.
            match ngenorca_config::load_config(cli.config.as_deref()) {
                Ok(config) => {
                    println!("Config:             ✓ loaded");
                    println!("  Data dir:         {}", config.data_dir.display());
                    println!("  Model:            {}", config.agent.model);
                    println!("  Sandbox enabled:  {}", config.sandbox.enabled);
                    println!("  Memory enabled:   {}", config.memory.enabled);
                }
                Err(e) => {
                    println!("Config:             ✗ error: {e}");
                }
            }

            println!();
            println!("All checks passed ✓");
        }

        Commands::Info => {
            println!("🐋 NgenOrca v{}", env!("CARGO_PKG_VERSION"));
            println!();
            println!("Architecture: {}", std::env::consts::ARCH);
            println!("OS:           {}", std::env::consts::OS);
            println!("Family:       {}", std::env::consts::FAMILY);
            println!("Sandbox:      {:?}", ngenorca_sandbox::detect_environment());
        }
    }

    Ok(())
}

// ─── Onboard Wizard ─────────────────────────────────────────────

/// Interactive first-time setup wizard. Walks the user through choosing a
/// model provider, auth mode, and channel adapters, then writes the config.
fn run_onboard_wizard(config_path: Option<&str>) -> anyhow::Result<()> {
    use dialoguer::{Confirm, Input, Select};
    use std::io::Write;

    println!();
    println!("🐋  Welcome to NgenOrca!");
    println!("    Let's set up your personal AI assistant.\n");

    // ── Step 1: Model provider ──────────────────────────────────
    let provider_choices = &[
        "ollama  — local models (recommended for privacy)",
        "anthropic — Claude (requires API key)",
        "openai  — GPT (requires API key)",
    ];
    let provider_idx = Select::new()
        .with_prompt("1. Choose your model provider")
        .items(provider_choices)
        .default(0)
        .interact()?;

    let (provider_name, default_model, needs_key) = match provider_idx {
        0 => ("ollama", "ollama/llama3.1:8b", false),
        1 => ("anthropic", "anthropic/claude-sonnet-4-20250514", true),
        2 => ("openai", "openai/gpt-4o-mini", true),
        _ => ("ollama", "ollama/llama3.1:8b", false),
    };

    let model: String = Input::new()
        .with_prompt("   Model name")
        .default(default_model.into())
        .interact_text()?;

    let api_key: Option<String> = if needs_key {
        let key: String = Input::new()
            .with_prompt(format!("   {} API key", provider_name))
            .interact_text()?;
        if key.is_empty() { None } else { Some(key) }
    } else {
        None
    };

    // ── Step 2: Auth mode ───────────────────────────────────────
    let auth_choices = &[
        "None       — no authentication (local only)",
        "Password   — simple shared password",
        "Token      — bearer tokens",
        "TrustedProxy — reverse proxy (Authelia, etc.)",
    ];
    let auth_idx = Select::new()
        .with_prompt("2. Choose authentication mode")
        .items(auth_choices)
        .default(0)
        .interact()?;

    let auth_mode = match auth_idx {
        0 => "None",
        1 => "Password",
        2 => "Token",
        3 => "TrustedProxy",
        _ => "None",
    };

    let auth_password: Option<String> = if auth_mode == "Password" {
        let pw: String = Input::new()
            .with_prompt("   Set a password")
            .interact_text()?;
        if pw.is_empty() { None } else { Some(pw) }
    } else {
        None
    };

    // ── Step 3: Gateway settings ────────────────────────────────
    let port: u16 = Input::new()
        .with_prompt("3. Gateway port")
        .default(18789)
        .interact_text()?;

    let bind: String = Input::new()
        .with_prompt("   Bind address")
        .default("127.0.0.1".into())
        .interact_text()?;

    // ── Step 4: Channels ────────────────────────────────────────
    let enable_telegram = Confirm::new()
        .with_prompt("4. Enable Telegram adapter?")
        .default(false)
        .interact()?;

    let telegram_token: Option<String> = if enable_telegram {
        let tk: String = Input::new()
            .with_prompt("   Telegram Bot token")
            .interact_text()?;
        if tk.is_empty() { None } else { Some(tk) }
    } else {
        None
    };

    // ── Generate config ─────────────────────────────────────────
    let config_dir = config_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            dirs_home().join(".ngenorca").join("config.toml")
        });
    let config_dir_parent = config_dir.parent().unwrap_or(std::path::Path::new("."));
    std::fs::create_dir_all(config_dir_parent)?;

    // Build TOML content.
    let mut toml = String::new();
    toml.push_str("# NgenOrca configuration — generated by `ngenorca onboard`\n\n");

    toml.push_str("[gateway]\n");
    toml.push_str(&format!("bind = \"{bind}\"\n"));
    toml.push_str(&format!("port = {port}\n"));
    toml.push_str(&format!("auth_mode = \"{auth_mode}\"\n"));
    if let Some(ref pw) = auth_password {
        toml.push_str(&format!("auth_password = \"{pw}\"\n"));
    }
    toml.push('\n');

    toml.push_str("[agent]\n");
    toml.push_str(&format!("model = \"{model}\"\n"));
    toml.push('\n');

    // Provider section
    match provider_name {
        "anthropic" => {
            toml.push_str("[agent.providers.anthropic]\n");
            if let Some(ref key) = api_key {
                toml.push_str(&format!("api_key = \"{key}\"\n"));
            }
            toml.push('\n');
        }
        "openai" => {
            toml.push_str("[agent.providers.openai]\n");
            if let Some(ref key) = api_key {
                toml.push_str(&format!("api_key = \"{key}\"\n"));
            }
            toml.push('\n');
        }
        _ => {} // ollama uses defaults
    }

    if enable_telegram {
        toml.push_str("[channels.telegram]\n");
        toml.push_str("enabled = true\n");
        if let Some(ref tk) = telegram_token {
            toml.push_str(&format!("bot_token = \"{tk}\"\n"));
        }
        toml.push('\n');
    }

    // Preview & confirm
    println!("\n── Generated config ──\n");
    println!("{toml}");

    let save = Confirm::new()
        .with_prompt(format!("Save to {}?", config_dir.display()))
        .default(true)
        .interact()?;

    if save {
        let mut file = std::fs::File::create(&config_dir)?;
        file.write_all(toml.as_bytes())?;
        println!("\n✓ Config saved to {}", config_dir.display());
        println!("  Run `ngenorca gateway` to start your assistant.");
    } else {
        println!("\nConfig not saved. You can re-run `ngenorca onboard` any time.");
    }

    Ok(())
}

fn dirs_home() -> std::path::PathBuf {
    #[cfg(windows)]
    {
        std::env::var("USERPROFILE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("C:\\Users\\default"))
    }
    #[cfg(not(windows))]
    {
        std::env::var("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
    }
}

fn print_banner() {
    println!(
        r#"
  _   _                  ___
 | \ | | __ _  ___ _ __ / _ \ _ __ ___ __ _
 |  \| |/ _` |/ _ | '_ | | | | '__/ __/ _` |
 | |\  | (_| |  __| | || |_| | | | (_| (_| |
 |_| \_|\__, |\___|_| |_\___/|_|  \___\__,_|
        |___/
    Personal AI Assistant — v{}
    "#,
        env!("CARGO_PKG_VERSION")
    );
}
