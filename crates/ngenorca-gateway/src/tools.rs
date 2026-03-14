//! Built-in agent tools.
//!
//! These tools give NgenOrca first-party capabilities for:
//! - reading and writing files in the configured workspace
//! - listing directories and searching workspace contents
//! - fetching URLs and performing lightweight web search
//! - running OS commands from the workspace

use async_trait::async_trait;
use ngenorca_config::NgenOrcaConfig;
use ngenorca_core::{Error, Result, SessionId, UserId};
use ngenorca_plugin_sdk::{AgentTool, ToolDefinition};
use reqwest::Client;
use serde_json::json;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tokio::process::Command;
use tracing::{debug, warn};

use crate::plugins::PluginRegistry;

const DEFAULT_HTTP_TIMEOUT_SECS: u64 = 15;
const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 30;
const MAX_COMMAND_TIMEOUT_SECS: u64 = 300;
const DEFAULT_FETCH_MAX_CHARS: usize = 12_000;
const DEFAULT_SEARCH_RESULTS: usize = 10;
const DEFAULT_WEB_RESULTS: usize = 5;
const DEFAULT_OUTPUT_MAX_CHARS: usize = 16_000;

pub async fn register_builtin_tools(registry: &PluginRegistry, config: &NgenOrcaConfig) {
    let workspace_root = config.agent.workspace.clone();
    let http_client = Client::builder()
        .timeout(std::time::Duration::from_secs(DEFAULT_HTTP_TIMEOUT_SECS))
        .user_agent(format!("NgenOrca/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_else(|_| Client::new());

    registry
        .register_tool(Arc::new(ListDirectoryTool::new(workspace_root.clone())))
        .await;
    registry
        .register_tool(Arc::new(ReadWorkspaceFileTool::new(workspace_root.clone())))
        .await;
    registry
        .register_tool(Arc::new(WriteWorkspaceFileTool::new(workspace_root.clone())))
        .await;
    registry
        .register_tool(Arc::new(GrepWorkspaceTool::new(workspace_root.clone())))
        .await;
    registry
        .register_tool(Arc::new(FetchUrlTool::new(http_client.clone())))
        .await;
    registry
        .register_tool(Arc::new(WebSearchTool::new(http_client.clone())))
        .await;
    registry
        .register_tool(Arc::new(RunCommandTool::new(workspace_root)))
        .await;
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new(std::path::MAIN_SEPARATOR_STR)),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }

    normalized
}

fn resolve_workspace_path(workspace_root: &Path, requested: &str) -> Result<PathBuf> {
    let workspace_root = normalize_path(workspace_root);
    let candidate = {
        let req_path = Path::new(requested);
        if req_path.is_absolute() {
            normalize_path(req_path)
        } else {
            normalize_path(&workspace_root.join(req_path))
        }
    };

    if candidate.starts_with(&workspace_root) {
        Ok(candidate)
    } else {
        Err(Error::Unauthorized(format!(
            "Path escapes workspace: {requested}"
        )))
    }
}

fn maybe_relativize(path: &Path, workspace_root: &Path) -> String {
    path.strip_prefix(workspace_root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn should_skip_dir(name: &OsStr) -> bool {
    matches!(
        name.to_string_lossy().as_ref(),
        ".git" | "target" | "node_modules" | ".idea" | ".vscode"
    )
}

fn truncate_chars(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }

    let mut out = input.chars().take(max_chars).collect::<String>();
    out.push_str("\n...[truncated]");
    out
}

async fn run_command_capture(
    command: &str,
    args: &[String],
    cwd: &Path,
    timeout_secs: u64,
) -> Result<serde_json::Value> {
    let timeout_secs = timeout_secs.clamp(1, MAX_COMMAND_TIMEOUT_SECS);
    let mut cmd = Command::new(command);
    cmd.args(args)
        .current_dir(cwd)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let result = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), cmd.output())
        .await;

    match result {
        Ok(Ok(output)) => Ok(json!({
            "command": command,
            "args": args,
            "cwd": cwd.to_string_lossy(),
            "exit_code": output.status.code().unwrap_or(-1),
            "success": output.status.success(),
            "stdout": truncate_chars(&String::from_utf8_lossy(&output.stdout), DEFAULT_OUTPUT_MAX_CHARS),
            "stderr": truncate_chars(&String::from_utf8_lossy(&output.stderr), DEFAULT_OUTPUT_MAX_CHARS),
            "timed_out": false,
        })),
        Ok(Err(e)) => Err(Error::Sandbox(format!("Failed to run command '{command}': {e}"))),
        Err(_) => Ok(json!({
            "command": command,
            "args": args,
            "cwd": cwd.to_string_lossy(),
            "exit_code": -1,
            "success": false,
            "stdout": "",
            "stderr": format!("Command timed out after {timeout_secs}s"),
            "timed_out": true,
        })),
    }
}

#[derive(Clone)]
struct WorkspaceTool {
    workspace_root: PathBuf,
}

impl WorkspaceTool {
    fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root: normalize_path(&workspace_root),
        }
    }

    fn resolve(&self, requested: &str) -> Result<PathBuf> {
        resolve_workspace_path(&self.workspace_root, requested)
    }
}

struct ListDirectoryTool {
    base: WorkspaceTool,
}

impl ListDirectoryTool {
    fn new(workspace_root: PathBuf) -> Self {
        Self {
            base: WorkspaceTool::new(workspace_root),
        }
    }
}

#[async_trait]
impl AgentTool for ListDirectoryTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "list_dir".into(),
            description: "List files and directories inside the configured workspace." .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative path inside the workspace. Defaults to workspace root." }
                },
                "additionalProperties": false
            }),
            requires_sandbox: false,
        }
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _session_id: &SessionId,
        _user_id: Option<&UserId>,
    ) -> Result<serde_json::Value> {
        let requested = arguments
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".");
        let dir = self.base.resolve(requested)?;

        let mut entries = Vec::new();
        for entry in std::fs::read_dir(&dir)
            .map_err(|e| Error::NotFound(format!("Cannot list directory '{}': {e}", dir.display())))?
        {
            let entry = entry.map_err(|e| Error::Gateway(format!("Directory entry error: {e}")))?;
            let meta = entry
                .metadata()
                .map_err(|e| Error::Gateway(format!("Metadata error: {e}")))?;
            let path = entry.path();
            entries.push(json!({
                "name": entry.file_name().to_string_lossy(),
                "path": maybe_relativize(&path, &self.base.workspace_root),
                "kind": if meta.is_dir() { "dir" } else if meta.is_file() { "file" } else { "other" },
                "size": if meta.is_file() { Some(meta.len()) } else { None::<u64> }
            }));
        }

        entries.sort_by(|a, b| {
            a.get("path")
                .and_then(|v| v.as_str())
                .cmp(&b.get("path").and_then(|v| v.as_str()))
        });

        Ok(json!({
            "path": maybe_relativize(&dir, &self.base.workspace_root),
            "entries": entries
        }))
    }
}

struct ReadWorkspaceFileTool {
    base: WorkspaceTool,
}

impl ReadWorkspaceFileTool {
    fn new(workspace_root: PathBuf) -> Self {
        Self {
            base: WorkspaceTool::new(workspace_root),
        }
    }
}

#[async_trait]
impl AgentTool for ReadWorkspaceFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_file".into(),
            description: "Read a text file from the configured workspace, optionally by line range.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "start_line": { "type": "integer", "minimum": 1 },
                    "end_line": { "type": "integer", "minimum": 1 }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            requires_sandbox: false,
        }
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _session_id: &SessionId,
        _user_id: Option<&UserId>,
    ) -> Result<serde_json::Value> {
        let path = arguments
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Gateway("Missing 'path'".into()))?;
        let file_path = self.base.resolve(path)?;
        let content = std::fs::read_to_string(&file_path).map_err(|e| {
            Error::NotFound(format!("Cannot read file '{}': {e}", file_path.display()))
        })?;

        let start_line = arguments
            .get("start_line")
            .and_then(|v| v.as_u64())
            .unwrap_or(1) as usize;
        let end_line = arguments
            .get("end_line")
            .and_then(|v| v.as_u64())
            .unwrap_or(usize::MAX as u64) as usize;

        let lines: Vec<&str> = content.lines().collect();
        let selected = lines
            .iter()
            .enumerate()
            .filter(|(idx, _)| {
                let line_no = idx + 1;
                line_no >= start_line && line_no <= end_line
            })
            .map(|(_, line)| *line)
            .collect::<Vec<_>>()
            .join("\n");

        Ok(json!({
            "path": maybe_relativize(&file_path, &self.base.workspace_root),
            "start_line": start_line,
            "end_line": end_line.min(lines.len()),
            "content": truncate_chars(&selected, DEFAULT_OUTPUT_MAX_CHARS)
        }))
    }
}

struct WriteWorkspaceFileTool {
    base: WorkspaceTool,
}

impl WriteWorkspaceFileTool {
    fn new(workspace_root: PathBuf) -> Self {
        Self {
            base: WorkspaceTool::new(workspace_root),
        }
    }
}

#[async_trait]
impl AgentTool for WriteWorkspaceFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write_file".into(),
            description: "Write or append text to a file inside the configured workspace.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" },
                    "append": { "type": "boolean" },
                    "create_dirs": { "type": "boolean" }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
            requires_sandbox: false,
        }
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _session_id: &SessionId,
        _user_id: Option<&UserId>,
    ) -> Result<serde_json::Value> {
        let path = arguments
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Gateway("Missing 'path'".into()))?;
        let content = arguments
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Gateway("Missing 'content'".into()))?;
        let append = arguments
            .get("append")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let create_dirs = arguments
            .get("create_dirs")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let file_path = self.base.resolve(path)?;
        if create_dirs {
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    Error::Gateway(format!("Cannot create parent directories '{}': {e}", parent.display()))
                })?;
            }
        }

        if append {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&file_path)
                .map_err(|e| Error::Gateway(format!("Cannot open file '{}': {e}", file_path.display())))?;
            file.write_all(content.as_bytes()).map_err(|e| {
                Error::Gateway(format!("Cannot append file '{}': {e}", file_path.display()))
            })?;
        } else {
            std::fs::write(&file_path, content).map_err(|e| {
                Error::Gateway(format!("Cannot write file '{}': {e}", file_path.display()))
            })?;
        }

        Ok(json!({
            "path": maybe_relativize(&file_path, &self.base.workspace_root),
            "bytes_written": content.len(),
            "append": append
        }))
    }
}

struct GrepWorkspaceTool {
    base: WorkspaceTool,
}

impl GrepWorkspaceTool {
    fn new(workspace_root: PathBuf) -> Self {
        Self {
            base: WorkspaceTool::new(workspace_root),
        }
    }
}

fn collect_text_matches(
    root: &Path,
    workspace_root: &Path,
    query: &str,
    case_sensitive: bool,
    max_results: usize,
    out: &mut Vec<serde_json::Value>,
) -> Result<()> {
    if out.len() >= max_results {
        return Ok(());
    }

    for entry in std::fs::read_dir(root)
        .map_err(|e| Error::Gateway(format!("Cannot read directory '{}': {e}", root.display())))?
    {
        if out.len() >= max_results {
            break;
        }

        let entry = entry.map_err(|e| Error::Gateway(format!("Directory entry error: {e}")))?;
        let path = entry.path();
        let file_name = entry.file_name();

        let metadata = match entry.metadata() {
            Ok(meta) => meta,
            Err(e) => {
                warn!(path = %path.display(), error = %e, "Skipping file with unreadable metadata");
                continue;
            }
        };

        if metadata.is_dir() {
            if should_skip_dir(&file_name) {
                continue;
            }
            collect_text_matches(&path, workspace_root, query, case_sensitive, max_results, out)?;
            continue;
        }

        if !metadata.is_file() || metadata.len() > 1_000_000 {
            continue;
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        for (idx, line) in content.lines().enumerate() {
            let matched = if case_sensitive {
                line.contains(query)
            } else {
                line.to_lowercase().contains(&query.to_lowercase())
            };

            if matched {
                out.push(json!({
                    "path": maybe_relativize(&path, workspace_root),
                    "line": idx + 1,
                    "text": truncate_chars(line, 300)
                }));

                if out.len() >= max_results {
                    return Ok(());
                }
            }
        }
    }

    Ok(())
}

#[async_trait]
impl AgentTool for GrepWorkspaceTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "grep_workspace".into(),
            description: "Search text recursively inside the configured workspace.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "path": { "type": "string" },
                    "case_sensitive": { "type": "boolean" },
                    "max_results": { "type": "integer", "minimum": 1, "maximum": 100 }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            requires_sandbox: false,
        }
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _session_id: &SessionId,
        _user_id: Option<&UserId>,
    ) -> Result<serde_json::Value> {
        let query = arguments
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Gateway("Missing 'query'".into()))?;
        let path = arguments
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".");
        let case_sensitive = arguments
            .get("case_sensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let max_results = arguments
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_SEARCH_RESULTS as u64) as usize;

        let root = self.base.resolve(path)?;
        let mut matches = Vec::new();
        collect_text_matches(
            &root,
            &self.base.workspace_root,
            query,
            case_sensitive,
            max_results,
            &mut matches,
        )?;

        Ok(json!({
            "query": query,
            "path": maybe_relativize(&root, &self.base.workspace_root),
            "matches": matches
        }))
    }
}

struct FetchUrlTool {
    client: Client,
}

impl FetchUrlTool {
    fn new(client: Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl AgentTool for FetchUrlTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "fetch_url".into(),
            description: "Fetch the contents of an HTTP or HTTPS URL.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string" },
                    "max_chars": { "type": "integer", "minimum": 100, "maximum": 50000 }
                },
                "required": ["url"],
                "additionalProperties": false
            }),
            requires_sandbox: false,
        }
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _session_id: &SessionId,
        _user_id: Option<&UserId>,
    ) -> Result<serde_json::Value> {
        let url = arguments
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Gateway("Missing 'url'".into()))?;
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(Error::Unauthorized("Only http:// and https:// URLs are allowed".into()));
        }

        let max_chars = arguments
            .get("max_chars")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_FETCH_MAX_CHARS as u64) as usize;

        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| Error::Gateway(format!("Failed to fetch URL '{url}': {e}")))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| Error::Gateway(format!("Failed to read response body: {e}")))?;

        Ok(json!({
            "url": url,
            "status": status.as_u16(),
            "content": truncate_chars(&body, max_chars)
        }))
    }
}

fn decode_html_entities(input: &str) -> String {
    input
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

fn strip_html_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;

    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }

    decode_html_entities(out.trim())
}

fn parse_duckduckgo_results(html: &str, max_results: usize) -> Vec<serde_json::Value> {
    let mut results = Vec::new();
    let mut rest = html;

    while results.len() < max_results {
        let Some(link_start) = rest.find("result__a") else {
            break;
        };
        rest = &rest[link_start..];

        let Some(href_idx) = rest.find("href=\"") else {
            break;
        };
        let href_part = &rest[href_idx + 6..];
        let Some(href_end) = href_part.find('"') else {
            break;
        };
        let url = &href_part[..href_end];

        let Some(title_start) = href_part[href_end..].find('>') else {
            break;
        };
        let title_part = &href_part[href_end + title_start + 1..];
        let Some(title_end) = title_part.find("</a>") else {
            break;
        };
        let title = strip_html_tags(&title_part[..title_end]);

        let mut snippet = String::new();
        if let Some(snippet_idx) = rest.find("result__snippet") {
            let snippet_rest = &rest[snippet_idx..];
            if let Some(open) = snippet_rest.find('>') {
                let snippet_part = &snippet_rest[open + 1..];
                if let Some(close) = snippet_part.find("</") {
                    snippet = strip_html_tags(&snippet_part[..close]);
                }
            }
        }

        let cleaned_url = url
            .replace("&amp;", "&")
            .replace("//duckduckgo.com/l/?uddg=", "");

        results.push(json!({
            "title": title,
            "url": cleaned_url,
            "snippet": snippet,
        }));

        rest = &title_part[title_end..];
    }

    results
}

struct WebSearchTool {
    client: Client,
}

impl WebSearchTool {
    fn new(client: Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl AgentTool for WebSearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "web_search".into(),
            description: "Search the public web for a query and return top results.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "max_results": { "type": "integer", "minimum": 1, "maximum": 10 }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            requires_sandbox: false,
        }
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _session_id: &SessionId,
        _user_id: Option<&UserId>,
    ) -> Result<serde_json::Value> {
        let query = arguments
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Gateway("Missing 'query'".into()))?;
        let max_results = arguments
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_WEB_RESULTS as u64) as usize;

        let url = format!(
            "https://html.duckduckgo.com/html/?q={}",
            urlencoding::encode(query)
        );

        let body = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Gateway(format!("Web search request failed: {e}")))?
            .text()
            .await
            .map_err(|e| Error::Gateway(format!("Web search response failed: {e}")))?;

        let results = parse_duckduckgo_results(&body, max_results);
        debug!(query, count = results.len(), "Web search completed");

        Ok(json!({
            "query": query,
            "results": results
        }))
    }
}

struct RunCommandTool {
    base: WorkspaceTool,
}

impl RunCommandTool {
    fn new(workspace_root: PathBuf) -> Self {
        Self {
            base: WorkspaceTool::new(workspace_root),
        }
    }
}

#[async_trait]
impl AgentTool for RunCommandTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "run_command".into(),
            description: "Run an OS command from the configured workspace. For shell built-ins, call powershell/cmd/sh explicitly.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "cwd": { "type": "string", "description": "Optional relative path inside the workspace." },
                    "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 300 }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
            requires_sandbox: true,
        }
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _session_id: &SessionId,
        _user_id: Option<&UserId>,
    ) -> Result<serde_json::Value> {
        let command = arguments
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Gateway("Missing 'command'".into()))?;
        let args = arguments
            .get("args")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(ToOwned::to_owned))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let timeout_secs = arguments
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_COMMAND_TIMEOUT_SECS);
        let cwd = arguments
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(|p| self.base.resolve(p))
            .transpose()?
            .unwrap_or_else(|| self.base.workspace_root.clone());

        run_command_capture(command, &args, &cwd, timeout_secs).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_workspace() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ngenorca_tools_test_{}_{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn resolve_workspace_path_blocks_escape() {
        let root = normalize_path(Path::new("/tmp/ngenorca-workspace"));
        assert!(resolve_workspace_path(&root, "subdir/file.txt").is_ok());
        assert!(resolve_workspace_path(&root, "../outside.txt").is_err());
    }

    #[tokio::test]
    async fn write_and_read_file_tool_roundtrip() {
        let workspace = temp_workspace();
        let write_tool = WriteWorkspaceFileTool::new(workspace.clone());
        let read_tool = ReadWorkspaceFileTool::new(workspace.clone());

        write_tool
            .execute(
                json!({
                    "path": "notes/test.txt",
                    "content": "hello\nworld",
                    "create_dirs": true
                }),
                &SessionId::new(),
                None,
            )
            .await
            .unwrap();

        let read_back = read_tool
            .execute(
                json!({
                    "path": "notes/test.txt",
                    "start_line": 2,
                    "end_line": 2
                }),
                &SessionId::new(),
                None,
            )
            .await
            .unwrap();

        assert_eq!(read_back["content"], "world");
    }

    #[tokio::test]
    async fn grep_workspace_finds_match() {
        let workspace = temp_workspace();
        std::fs::write(workspace.join("a.txt"), "alpha\nbeta\ngamma").unwrap();
        let tool = GrepWorkspaceTool::new(workspace);

        let result = tool
            .execute(json!({ "query": "beta" }), &SessionId::new(), None)
            .await
            .unwrap();

        assert_eq!(result["matches"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn list_dir_returns_entries() {
        let workspace = temp_workspace();
        std::fs::write(workspace.join("file.txt"), "x").unwrap();
        let tool = ListDirectoryTool::new(workspace);
        let result = tool
            .execute(json!({}), &SessionId::new(), None)
            .await
            .unwrap();

        assert!(!result["entries"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn run_command_returns_output() {
        let workspace = temp_workspace();
        let tool = RunCommandTool::new(workspace);

        #[cfg(windows)]
        let args = json!({ "command": "cmd", "args": ["/C", "echo", "hello"] });
        #[cfg(not(windows))]
        let args = json!({ "command": "sh", "args": ["-c", "echo hello"] });

        let result = tool.execute(args, &SessionId::new(), None).await.unwrap();
        assert!(result["stdout"].as_str().unwrap().to_lowercase().contains("hello"));
    }
}