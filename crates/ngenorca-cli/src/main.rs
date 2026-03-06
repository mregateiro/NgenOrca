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
            println!("Checking gateway status...");
            // TODO: Connect to running gateway and fetch /health.
            println!("(Not yet implemented — gateway HTTP client needed)");
        }

        Commands::Onboard => {
            println!("🐋 Welcome to NgenOrca!");
            println!();
            println!("Let's set up your personal AI assistant.");
            println!();
            println!("Step 1: Choose your default model");
            println!("  Supported: anthropic/claude-*, openai/gpt-*, ollama/*");
            println!();
            println!("(Interactive onboard wizard coming soon)");
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
