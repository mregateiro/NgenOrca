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

pub mod telegram;
pub mod webchat;

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
    if channels
        .webchat
        .as_ref()
        .is_some_and(|c| c.enabled)
    {
        let adapter = webchat::WebChatAdapter::new();
        registry
            .register(Box::new(adapter), serde_json::json!({}))
            .await?;
        info!("WebChat adapter registered");
    }

    // Telegram Bot adapter.
    if let Some(tg_cfg) = &channels.telegram {
        if tg_cfg.enabled {
            if let Some(token) = &tg_cfg.bot_token {
                let adapter = telegram::TelegramAdapter::new(
                    token.clone(),
                    tg_cfg.polling,
                    tg_cfg.webhook_url.clone(),
                    tg_cfg.allowed_users.clone(),
                );
                let cfg_json = serde_json::to_value(tg_cfg).unwrap_or_default();
                registry.register(Box::new(adapter), cfg_json).await?;
                info!("Telegram adapter registered");
            } else {
                tracing::warn!("Telegram enabled but no bot_token configured — skipping");
            }
        }
    }

    // TODO: Discord, WhatsApp, Slack, Signal, Matrix, Teams adapters
    // Each follows the same pattern: check config → create → register.

    Ok(())
}
