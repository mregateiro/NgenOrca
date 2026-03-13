//! Browser-based configuration editor for the persisted TOML file.

use axum::{Json, extract::State, http::StatusCode, response::Html};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct ConfigFileResponse {
    config_path: String,
    exists: bool,
    generated_from_runtime: bool,
    content: String,
}

#[derive(Debug, Deserialize)]
pub struct ConfigSaveRequest {
    content: String,
}

#[derive(Debug, Serialize)]
pub struct ConfigSaveResponse {
    config_path: String,
    backup_path: Option<String>,
    restart_required: bool,
    message: String,
}

pub async fn config_page(State(state): State<AppState>) -> Html<String> {
    Html(render_config_page(state.config_file_path()))
}

pub async fn get_config_file(
    State(state): State<AppState>,
) -> Result<Json<ConfigFileResponse>, (StatusCode, Json<Value>)> {
    let path = state.config_file_path();
    let exists = path.exists();
    let content = read_config_content(path, &state)?;

    Ok(Json(ConfigFileResponse {
        config_path: path.display().to_string(),
        exists,
        generated_from_runtime: !exists,
        content,
    }))
}

pub async fn get_effective_config(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    serde_json::to_value(state.config())
        .map(Json)
        .map_err(|e| internal_error(format!("Failed to serialize effective config: {e}")))
}

pub async fn save_config_file(
    State(state): State<AppState>,
    Json(payload): Json<ConfigSaveRequest>,
) -> Result<Json<ConfigSaveResponse>, (StatusCode, Json<Value>)> {
    let config_path = state.config_file_path().to_path_buf();

    if payload.content.trim().is_empty() {
        return Err(bad_request("Config file content must not be empty"));
    }

    validate_candidate_config(&config_path, &payload.content)?;
    let backup_path = write_config_with_backup(&config_path, &payload.content)?;

    Ok(Json(ConfigSaveResponse {
        config_path: config_path.display().to_string(),
        backup_path: backup_path.map(|p| p.display().to_string()),
        restart_required: true,
        message: "Configuration saved. Restart NgenOrca to apply the new settings.".into(),
    }))
}

fn read_config_content(path: &Path, state: &AppState) -> Result<String, (StatusCode, Json<Value>)> {
    if path.exists() {
        std::fs::read_to_string(path).map_err(|e| {
            internal_error(format!(
                "Failed to read config file {}: {e}",
                path.display()
            ))
        })
    } else {
        toml::to_string_pretty(state.config())
            .map_err(|e| internal_error(format!("Failed to render active config as TOML: {e}")))
    }
}

fn validate_candidate_config(
    config_path: &Path,
    content: &str,
) -> Result<(), (StatusCode, Json<Value>)> {
    let parent = config_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&parent).map_err(|e| {
        internal_error(format!(
            "Failed to create config directory {}: {e}",
            parent.display()
        ))
    })?;

    let validate_path = parent.join(format!(
        ".ngenorca-config-validate-{}-{}.toml",
        std::process::id(),
        Utc::now().timestamp_micros()
    ));

    std::fs::write(&validate_path, content).map_err(|e| {
        internal_error(format!(
            "Failed to stage config validation file {}: {e}",
            validate_path.display()
        ))
    })?;

    let validate_path_str = validate_path.to_string_lossy().into_owned();
    let result = ngenorca_config::load_config(Some(&validate_path_str));
    let _ = std::fs::remove_file(&validate_path);

    result
        .map(|_| ())
        .map_err(|e| bad_request(format!("Config validation failed: {e}")))
}

fn write_config_with_backup(
    config_path: &Path,
    content: &str,
) -> Result<Option<PathBuf>, (StatusCode, Json<Value>)> {
    let parent = config_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&parent).map_err(|e| {
        internal_error(format!(
            "Failed to create config directory {}: {e}",
            parent.display()
        ))
    })?;

    let backup_path = if config_path.exists() {
        let backup = parent.join(format!(
            "config.backup-{}.toml",
            Utc::now().format("%Y%m%dT%H%M%SZ")
        ));
        std::fs::copy(config_path, &backup).map_err(|e| {
            internal_error(format!(
                "Failed to create config backup {}: {e}",
                backup.display()
            ))
        })?;
        Some(backup)
    } else {
        None
    };

    let temp_path = parent.join(format!(
        ".config-write-{}-{}.tmp",
        std::process::id(),
        Utc::now().timestamp_micros()
    ));
    std::fs::write(&temp_path, content).map_err(|e| {
        internal_error(format!(
            "Failed to write temporary config file {}: {e}",
            temp_path.display()
        ))
    })?;

    if config_path.exists() {
        std::fs::remove_file(config_path).map_err(|e| {
            internal_error(format!(
                "Failed to replace config file {}: {e}",
                config_path.display()
            ))
        })?;
    }

    std::fs::rename(&temp_path, config_path).map_err(|e| {
        internal_error(format!(
            "Failed to move config file into place {}: {e}",
            config_path.display()
        ))
    })?;

    Ok(backup_path)
}

fn bad_request(message: impl Into<String>) -> (StatusCode, Json<Value>) {
    error_response(StatusCode::BAD_REQUEST, message)
}

fn internal_error(message: impl Into<String>) -> (StatusCode, Json<Value>) {
    error_response(StatusCode::INTERNAL_SERVER_ERROR, message)
}

fn error_response(status: StatusCode, message: impl Into<String>) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": message.into() })))
}

fn render_config_page(config_path: &Path) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>NgenOrca Config</title>
  <style>
    :root {{
      color-scheme: dark;
      --bg: #0b1020;
      --panel: #121a2f;
      --panel-2: #1a2440;
      --text: #e5ecff;
      --muted: #9cb0de;
      --accent: #6ea8fe;
      --success: #35c759;
      --danger: #ff6b6b;
      --border: #273250;
    }}
    * {{ box-sizing: border-box; }}
    body {{ margin: 0; font-family: Inter, Segoe UI, Arial, sans-serif; background: linear-gradient(180deg, #0b1020, #121a2f); color: var(--text); }}
    .wrap {{ max-width: 1200px; margin: 0 auto; padding: 24px; }}
    .grid {{ display: grid; gap: 16px; grid-template-columns: 1.2fr 0.8fr; }}
    .card {{ background: rgba(18, 26, 47, 0.92); border: 1px solid var(--border); border-radius: 16px; padding: 18px; box-shadow: 0 12px 40px rgba(0,0,0,0.25); }}
    h1, h2 {{ margin-top: 0; }}
    p, li {{ color: var(--muted); line-height: 1.5; }}
    textarea {{ width: 100%; min-height: 65vh; resize: vertical; background: #09101f; color: var(--text); border: 1px solid var(--border); border-radius: 12px; padding: 16px; font: 13px/1.5 Consolas, Monaco, monospace; }}
    pre {{ margin: 0; white-space: pre-wrap; word-break: break-word; background: #09101f; border: 1px solid var(--border); border-radius: 12px; padding: 16px; max-height: 65vh; overflow: auto; color: #d8e2ff; }}
    .row {{ display: flex; gap: 12px; flex-wrap: wrap; align-items: center; }}
    button {{ border: 0; border-radius: 10px; padding: 10px 16px; font-weight: 600; cursor: pointer; }}
    button.primary {{ background: var(--accent); color: #08101d; }}
    button.secondary {{ background: var(--panel-2); color: var(--text); border: 1px solid var(--border); }}
    .status {{ min-height: 24px; font-weight: 600; }}
    .status.ok {{ color: var(--success); }}
    .status.err {{ color: var(--danger); }}
    .mono {{ font-family: Consolas, Monaco, monospace; color: var(--text); }}
    .badge {{ display: inline-block; border: 1px solid var(--border); border-radius: 999px; padding: 4px 10px; color: var(--muted); background: #0d1528; }}
    @media (max-width: 980px) {{ .grid {{ grid-template-columns: 1fr; }} textarea, pre {{ min-height: 42vh; }} }}
  </style>
</head>
<body>
  <div class="wrap">
    <div class="card" style="margin-bottom: 16px;">
      <h1>NgenOrca configuration</h1>
      <p>Edit the persisted TOML config file, then restart the service to apply changes. This page manages the external config file, not repo files.</p>
      <div class="row">
        <span class="badge">Managed file: <span class="mono" id="configPath">{}</span></span>
        <span class="badge">Backups are created before overwriting an existing file</span>
      </div>
    </div>

    <div class="grid">
      <section class="card">
        <div class="row" style="justify-content: space-between; margin-bottom: 12px;">
          <h2>Config file</h2>
          <div class="row">
            <button class="secondary" id="reloadBtn" type="button">Reload</button>
            <button class="primary" id="saveBtn" type="button">Save config</button>
          </div>
        </div>
        <textarea id="editor" spellcheck="false"></textarea>
        <p class="status" id="status"></p>
      </section>

      <aside class="card">
        <h2>Effective config preview</h2>
        <p>This preview shows the currently running configuration. Saving the file does not hot-reload the gateway.</p>
        <pre id="effective">Loading…</pre>
      </aside>
    </div>
  </div>

  <script>
    const editor = document.getElementById('editor');
    const statusEl = document.getElementById('status');
    const effectiveEl = document.getElementById('effective');
    const configPathEl = document.getElementById('configPath');

    function setStatus(message, ok) {{
      statusEl.textContent = message;
      statusEl.className = 'status ' + (ok ? 'ok' : 'err');
    }}

    async function loadFile() {{
      const response = await fetch('/api/v1/config/file');
      const data = await response.json();
      if (!response.ok) {{
        throw new Error(data.error || 'Failed to load config file');
      }}
      editor.value = data.content;
      configPathEl.textContent = data.config_path;
      setStatus(data.exists ? 'Loaded persisted config file.' : 'No config file exists yet. Showing the active runtime config as a starting point.', true);
    }}

    async function loadEffective() {{
      const response = await fetch('/api/v1/config/effective');
      const data = await response.json();
      if (!response.ok) {{
        throw new Error(data.error || 'Failed to load effective config');
      }}
      effectiveEl.textContent = JSON.stringify(data, null, 2);
    }}

    async function saveFile() {{
      setStatus('Saving config…', true);
      const response = await fetch('/api/v1/config/file', {{
        method: 'PUT',
        headers: {{ 'Content-Type': 'application/json' }},
        body: JSON.stringify({{ content: editor.value }})
      }});
      const data = await response.json();
      if (!response.ok) {{
        throw new Error(data.error || 'Failed to save config');
      }}
      const backup = data.backup_path ? ' Backup: ' + data.backup_path + '.' : '';
      setStatus(data.message + backup, true);
    }}

    async function refreshAll() {{
      try {{
        await Promise.all([loadFile(), loadEffective()]);
      }} catch (error) {{
        setStatus(error.message, false);
      }}
    }}

    document.getElementById('reloadBtn').addEventListener('click', refreshAll);
    document.getElementById('saveBtn').addEventListener('click', async () => {{
      try {{
        await saveFile();
      }} catch (error) {{
        setStatus(error.message, false);
      }}
    }});

    refreshAll();
  </script>
</body>
</html>
"#,
        config_path.display()
    )
}
