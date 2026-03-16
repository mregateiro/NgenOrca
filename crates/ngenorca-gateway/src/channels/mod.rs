//! Channel adapter runtime — bridges external messaging platforms.
//!
//! Each channel adapter implements `Plugin + ChannelAdapter` from the SDK.
//! The gateway:
//! 1. Creates adapters from config
//! 2. Registers them in the PluginRegistry
//! 3. Starts their listener loops
//!
//! Built-in adapters:
//! - **WebChat** — served directly by the gateway (WebSocket), no adapter needed
//! - **Telegram** — long-polling against the Telegram Bot API
//! - **Discord** — Discord Bot via gateway events
//! - **Slack** — Slack Bot (Socket Mode or Webhook)
//! - **WhatsApp** — WhatsApp Cloud API
//! - **Signal** — signal-cli JSON-RPC backend
//! - **Matrix** — Matrix Client–Server API
//! - **Teams** — Microsoft Bot Framework

pub mod discord;
pub mod matrix;
pub mod signal;
pub mod slack;
pub mod teams;
pub mod telegram;
pub mod webchat;
pub mod whatsapp;

use crate::plugins::PluginRegistry;
use ngenorca_config::NgenOrcaConfig;
use tracing::info;

/// Initialize and register channel adapters based on config.
///
/// This reads the `channels` section of the config and creates
/// the appropriate adapter for each enabled channel.
pub async fn register_adapters(
    config: &NgenOrcaConfig,
    registry: &PluginRegistry,
) -> ngenorca_core::Result<()> {
    let channels = &config.channels;

    // WebChat is built-in (served via WebSocket routes) — register a lightweight
    // adapter so it appears in the plugin list and health checks.
    if channels.webchat.as_ref().is_some_and(|c| c.enabled) {
        let adapter = webchat::WebChatAdapter::new();
        registry
            .register_channel_adapter(Box::new(adapter), serde_json::json!({}))
            .await?;
        info!("WebChat adapter registered");
    }

    // Telegram Bot adapter.
    if let Some(tg_cfg) = &channels.telegram
        && tg_cfg.enabled
    {
        if let Some(token) = &tg_cfg.bot_token {
            let adapter = telegram::TelegramAdapter::new(
                token.clone(),
                tg_cfg.polling,
                tg_cfg.webhook_url.clone(),
                tg_cfg.allowed_users.clone(),
            );
            let cfg_json = serde_json::to_value(tg_cfg).unwrap_or_default();
            registry
                .register_channel_adapter(Box::new(adapter), cfg_json)
                .await?;
            info!("Telegram adapter registered");
        } else {
            tracing::warn!("Telegram enabled but no bot_token configured — skipping");
        }
    }

    // Discord Bot adapter.
    if let Some(dc) = &channels.discord
        && dc.enabled
    {
        if let Some(token) = &dc.bot_token {
            let adapter = discord::DiscordAdapter::new(
                token.clone(),
                dc.guild_ids.clone(),
                dc.command_prefix.clone().or_else(|| Some("!".into())),
            );
            let cfg_json = serde_json::to_value(dc).unwrap_or_default();
            registry
                .register_channel_adapter(Box::new(adapter), cfg_json)
                .await?;
            info!("Discord adapter registered");
        } else {
            tracing::warn!("Discord enabled but no bot_token configured — skipping");
        }
    }

    // Slack Bot adapter.
    if let Some(sl) = &channels.slack
        && sl.enabled
    {
        if let Some(token) = &sl.bot_token {
            let adapter = slack::SlackAdapter::new(
                token.clone(),
                sl.app_token.clone(),
                sl.socket_mode,
                sl.signing_secret.clone(),
            );
            let cfg_json = serde_json::to_value(sl).unwrap_or_default();
            registry
                .register_channel_adapter(Box::new(adapter), cfg_json)
                .await?;
            info!("Slack adapter registered");
        } else {
            tracing::warn!("Slack enabled but no bot_token configured — skipping");
        }
    }

    // WhatsApp adapter (Native pure-Rust or Cloud API).
    if let Some(wa) = &channels.whatsapp
        && wa.enabled
    {
        // Cloud API mode when access_token is provided.
        if let Some(token) = &wa.access_token {
            let adapter = whatsapp::WhatsAppAdapter::cloud_api(
                wa.phone_number_id.clone().unwrap_or_default(),
                token.clone(),
                wa.verify_token.clone().unwrap_or_default(),
                wa.webhook_path.clone(),
                wa.app_secret.clone(),
            );
            let cfg_json = serde_json::to_value(wa).unwrap_or_default();
            registry
                .register_channel_adapter(Box::new(adapter), cfg_json)
                .await?;
            info!("WhatsApp adapter registered (Cloud API mode)");
        } else {
            // Native mode (default) — pure Rust, no Node.js needed.
            let data_dir = wa.data_path.clone().unwrap_or_else(|| {
                let home = std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .unwrap_or_else(|_| ".".into());
                let mut p = std::path::PathBuf::from(home);
                p.push(".ngenorca");
                p.push("whatsapp-data");
                p
            });
            let adapter = whatsapp::WhatsAppAdapter::native(data_dir);
            let cfg_json = serde_json::to_value(wa).unwrap_or_default();
            registry
                .register_channel_adapter(Box::new(adapter), cfg_json)
                .await?;
            info!("WhatsApp adapter registered (Native mode — pure Rust)");
        }
    }

    // Signal adapter (via signal-cli).
    if let Some(sg) = &channels.signal
        && sg.enabled
    {
        if let Some(phone) = &sg.phone_number {
            let adapter = signal::SignalAdapter::new(
                phone.clone(),
                sg.signal_cli_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "signal-cli".into()),
                sg.data_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "/var/lib/signal-cli".into()),
                sg.mode.clone(),
            );
            let cfg_json = serde_json::to_value(sg).unwrap_or_default();
            registry
                .register_channel_adapter(Box::new(adapter), cfg_json)
                .await?;
            info!("Signal adapter registered");
        } else {
            tracing::warn!("Signal enabled but no phone_number configured — skipping");
        }
    }

    // Matrix adapter.
    if let Some(mx) = &channels.matrix
        && mx.enabled
    {
        if let Some(token) = &mx.access_token {
            let adapter = matrix::MatrixAdapter::new(
                mx.homeserver
                    .clone()
                    .unwrap_or_else(|| "https://matrix.org".into()),
                mx.user_id.clone().unwrap_or_default().to_string(),
                token.clone(),
                mx.device_id.clone(),
                mx.auto_join,
                mx.encrypted,
            );
            let cfg_json = serde_json::to_value(mx).unwrap_or_default();
            registry
                .register_channel_adapter(Box::new(adapter), cfg_json)
                .await?;
            info!("Matrix adapter registered");
        } else {
            tracing::warn!("Matrix enabled but no access_token configured — skipping");
        }
    }

    // Microsoft Teams adapter.
    if let Some(tm) = &channels.teams
        && tm.enabled
    {
        if let Some(app_id) = &tm.app_id {
            let adapter = teams::TeamsAdapter::new(
                app_id.clone(),
                tm.app_password.clone().unwrap_or_default(),
                Some(tm.tenant_id.clone()),
                tm.webhook_url.clone(),
            );
            let cfg_json = serde_json::to_value(tm).unwrap_or_default();
            registry
                .register_channel_adapter(Box::new(adapter), cfg_json)
                .await?;
            info!("Teams adapter registered");
            tracing::info!(
                "SEC-05: Teams webhook uses full Bot Framework JWKS JWT validation \
                     (issuer, audience, expiry, and RSA signature are verified)."
            );
        } else {
            tracing::warn!("Teams enabled but no app_id configured — skipping");
        }
    }

    Ok(())
}
