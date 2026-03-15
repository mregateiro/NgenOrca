//! Built-in agent tools.
//!
//! These tools give NgenOrca first-party capabilities for:
//! - reading and writing files in the configured workspace
//! - listing directories and searching workspace contents
//! - fetching URLs and performing lightweight web search
//! - running OS commands from the workspace

use async_trait::async_trait;
use ngenorca_config::{NgenOrcaConfig, SandboxConfig};
use ngenorca_core::{Error, Result, SessionId, UserId};
use ngenorca_plugin_sdk::{
    AgentTool, AutomationStep, SkillArtifact, SkillArtifactStatus, ToolDefinition,
};
use reqwest::Client;
use serde_json::json;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, warn};

use crate::plugins::PluginRegistry;
use crate::skills::{
    SkillCheckpointRecord, SkillExecutionJournal, SkillExecutionStatus, SkillExecutionStepRecord,
    SkillRollbackPlanEntry, SkillStore, new_skill_execution_journal_id,
    skill_execution_status_label, synthesize_rollback_plan, synthesize_skill_script,
    validate_skill_artifact,
};

const DEFAULT_HTTP_TIMEOUT_SECS: u64 = 15;
const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 30;
const MAX_COMMAND_TIMEOUT_SECS: u64 = 300;
const DEFAULT_FETCH_MAX_CHARS: usize = 12_000;
const DEFAULT_SEARCH_RESULTS: usize = 10;
const DEFAULT_WEB_RESULTS: usize = 5;
const DEFAULT_OUTPUT_MAX_CHARS: usize = 16_000;

pub async fn register_builtin_tools(registry: &PluginRegistry, config: &NgenOrcaConfig) {
    let workspace_root = config.agent.workspace.clone();
    let skill_store = SkillStore::new(config.data_dir.join("skills"));
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
        .register_tool(Arc::new(WriteWorkspaceFileTool::new(
            workspace_root.clone(),
        )))
        .await;
    registry
        .register_tool(Arc::new(GrepWorkspaceTool::new(workspace_root.clone())))
        .await;
    registry
        .register_tool(Arc::new(ListSkillsTool::new(skill_store.clone())))
        .await;
    registry
        .register_tool(Arc::new(ReadSkillTool::new(skill_store.clone())))
        .await;
    registry
        .register_tool(Arc::new(ValidateSkillTool::new(skill_store.clone())))
        .await;
    registry
        .register_tool(Arc::new(SynthesizeSkillScriptTool::new(
            skill_store.clone(),
        )))
        .await;
    registry
        .register_tool(Arc::new(ExecuteSkillStagesTool::new(
            skill_store.clone(),
            config.agent.workspace.clone(),
            config.sandbox.clone(),
        )))
        .await;
    registry
        .register_tool(Arc::new(SaveSkillTool::new(skill_store)))
        .await;
    registry
        .register_tool(Arc::new(FetchUrlTool::new(http_client.clone())))
        .await;
    registry
        .register_tool(Arc::new(WebSearchTool::new(http_client.clone())))
        .await;
    registry
        .register_tool(Arc::new(RunCommandTool::new(
            workspace_root,
            config.sandbox.clone(),
        )))
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

fn failed_command_result(
    command: &str,
    args: &[String],
    cwd: &Path,
    sandbox: &SandboxConfig,
    workspace_root: &Path,
    timeout_secs: u64,
    message: &str,
) -> serde_json::Value {
    json!({
        "command": command,
        "args": args,
        "cwd": cwd.to_string_lossy(),
        "exit_code": serde_json::Value::Null,
        "success": false,
        "stdout": "",
        "stderr": truncate_chars(message, DEFAULT_OUTPUT_MAX_CHARS),
        "timed_out": false,
        "sandboxed": sandbox.enabled,
        "sandbox_environment": format!("{:?}", ngenorca_sandbox::detect_environment()),
        "sandbox_policy": sandbox_policy_summary(&command_sandbox_policy(timeout_secs, workspace_root, sandbox), workspace_root),
        "sandbox_audit": serde_json::Value::Null,
    })
}

async fn run_command_capture(
    command: &str,
    args: &[String],
    cwd: &Path,
    timeout_secs: u64,
    workspace_root: &Path,
    sandbox: &SandboxConfig,
) -> Result<serde_json::Value> {
    let policy = command_sandbox_policy(timeout_secs, workspace_root, sandbox);
    let string_args: Vec<&str> = args.iter().map(|arg| arg.as_str()).collect();
    let output = if sandbox.enabled {
        ngenorca_sandbox::sandboxed_exec_with_cwd(command, &string_args, Some(cwd), &policy).await
    } else {
        ngenorca_sandbox::unsandboxed_exec_with_cwd(command, &string_args, Some(cwd), &policy).await
    }
    .map_err(|e| Error::Sandbox(format!("Failed to run command '{command}': {e}")))?;

    Ok(json!({
        "command": command,
        "args": args,
        "cwd": cwd.to_string_lossy(),
        "exit_code": output.exit_code,
        "success": output.exit_code == 0,
        "stdout": truncate_chars(&output.stdout, DEFAULT_OUTPUT_MAX_CHARS),
        "stderr": truncate_chars(&output.stderr, DEFAULT_OUTPUT_MAX_CHARS),
        "timed_out": output.timed_out,
        "sandboxed": sandbox.enabled,
        "sandbox_environment": format!("{:?}", ngenorca_sandbox::detect_environment()),
        "sandbox_policy": sandbox_policy_summary(&policy, workspace_root),
        "sandbox_audit": output.audit,
    }))
}

fn command_sandbox_policy(
    timeout_secs: u64,
    workspace_root: &Path,
    sandbox: &SandboxConfig,
) -> ngenorca_sandbox::SandboxPolicy {
    let requested_timeout_secs = timeout_secs.clamp(1, MAX_COMMAND_TIMEOUT_SECS);
    let workspace_root = workspace_root.to_string_lossy().to_string();

    let mut allow_read_paths = vec![workspace_root.clone()];
    allow_read_paths.extend(sandbox.policy.additional_read_paths.clone());

    let mut allow_write_paths = sandbox.policy.additional_write_paths.clone();
    if sandbox.policy.allow_workspace_write {
        allow_write_paths.insert(0, workspace_root.clone());
    }

    ngenorca_sandbox::SandboxPolicy {
        allow_network: sandbox.policy.allow_network,
        allow_read_paths,
        allow_write_paths,
        allow_spawn: sandbox.policy.allow_child_processes,
        memory_limit_bytes: megabytes_to_bytes(sandbox.policy.memory_limit_mb),
        cpu_time_limit_secs: apply_timeout_cap(
            requested_timeout_secs,
            sandbox.policy.cpu_limit_seconds,
        ),
        wall_timeout_secs: apply_timeout_cap(
            requested_timeout_secs,
            sandbox.policy.wall_time_limit_seconds,
        ),
    }
}

fn sandbox_policy_summary(
    policy: &ngenorca_sandbox::SandboxPolicy,
    workspace_root: &Path,
) -> serde_json::Value {
    json!({
        "allow_network": policy.allow_network,
        "allow_spawn": policy.allow_spawn,
        "workspace_root": workspace_root.to_string_lossy().to_string(),
        "allow_read_paths": policy.allow_read_paths.clone(),
        "allow_write_paths": policy.allow_write_paths.clone(),
        "memory_limit_mb": bytes_to_megabytes(policy.memory_limit_bytes),
        "cpu_limit_seconds": policy.cpu_time_limit_secs,
        "wall_time_limit_seconds": policy.wall_timeout_secs,
    })
}

fn megabytes_to_bytes(megabytes: u64) -> u64 {
    megabytes.saturating_mul(1024 * 1024)
}

fn bytes_to_megabytes(bytes: u64) -> u64 {
    bytes / (1024 * 1024)
}

fn apply_timeout_cap(requested: u64, configured: u64) -> u64 {
    match configured {
        0 => requested,
        configured => requested.min(configured),
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
            description: "List files and directories inside the configured workspace.".into(),
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
        for entry in std::fs::read_dir(&dir).map_err(|e| {
            Error::NotFound(format!("Cannot list directory '{}': {e}", dir.display()))
        })? {
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
            description:
                "Read a text file from the configured workspace, optionally by line range.".into(),
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
        if create_dirs && let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::Gateway(format!(
                    "Cannot create parent directories '{}': {e}",
                    parent.display()
                ))
            })?;
        }

        if append {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&file_path)
                .map_err(|e| {
                    Error::Gateway(format!("Cannot open file '{}': {e}", file_path.display()))
                })?;
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
            collect_text_matches(
                &path,
                workspace_root,
                query,
                case_sensitive,
                max_results,
                out,
            )?;
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
            return Err(Error::Unauthorized(
                "Only http:// and https:// URLs are allowed".into(),
            ));
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
    sandbox: SandboxConfig,
}

impl RunCommandTool {
    fn new(workspace_root: PathBuf, sandbox: SandboxConfig) -> Self {
        Self {
            base: WorkspaceTool::new(workspace_root),
            sandbox,
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

        run_command_capture(
            command,
            &args,
            &cwd,
            timeout_secs,
            &self.base.workspace_root,
            &self.sandbox,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ngenorca_plugin_sdk::AutomationStep;

    fn temp_workspace() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ngenorca_tools_test_{}_{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(windows)]
    fn successful_command_args() -> serde_json::Value {
        json!({ "command": "cmd", "args": ["/C", "echo", "hello"] })
    }

    #[cfg(not(windows))]
    fn successful_command_args() -> serde_json::Value {
        json!({ "command": "sh", "args": ["-c", "echo hello"] })
    }

    fn tool_test_sandbox_config() -> SandboxConfig {
        SandboxConfig {
            enabled: false,
            ..SandboxConfig::default()
        }
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
        let tool = RunCommandTool::new(workspace, tool_test_sandbox_config());

        let args = successful_command_args();

        let result = tool.execute(args, &SessionId::new(), None).await.unwrap();
        assert!(
            result["stdout"]
                .as_str()
                .unwrap()
                .to_lowercase()
                .contains("hello")
        );
        assert_eq!(result["sandboxed"], false);
        assert_eq!(result["sandbox_policy"]["allow_network"], false);
        assert!(result["sandbox_audit"]["backend"].is_string());
    }

    #[tokio::test]
    async fn run_command_honors_disabled_sandbox() {
        let workspace = temp_workspace();
        let sandbox = SandboxConfig {
            enabled: false,
            ..SandboxConfig::default()
        };
        let tool = RunCommandTool::new(workspace, sandbox);

        let args = successful_command_args();

        let result = tool.execute(args, &SessionId::new(), None).await.unwrap();
        assert_eq!(result["sandboxed"], false);
        assert_eq!(result["sandbox_audit"]["backend"], "direct");
        assert!(result["sandbox_audit"]["fallback_reason"].is_string());
    }

    #[tokio::test]
    async fn run_command_respects_cwd() {
        let workspace = temp_workspace();
        let nested = workspace.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let tool = RunCommandTool::new(workspace, tool_test_sandbox_config());

        #[cfg(windows)]
        let args = json!({ "command": "cmd", "args": ["/C", "cd"], "cwd": "nested" });
        #[cfg(not(windows))]
        let args = json!({ "command": "pwd", "cwd": "nested" });

        let result = tool.execute(args, &SessionId::new(), None).await.unwrap();
        let stdout = result["stdout"].as_str().unwrap().replace('\\', "/");
        assert!(stdout.contains("nested"));
    }

    #[tokio::test]
    async fn skill_tools_roundtrip() {
        let store = SkillStore::new(temp_workspace().join("skills"));
        let save_tool = SaveSkillTool::new(store.clone());
        let list_tool = ListSkillsTool::new(store.clone());
        let read_tool = ReadSkillTool::new(store.clone());
        let validate_tool = ValidateSkillTool::new(store);

        let validation = validate_tool
            .execute(
                json!({
                    "skill": {
                        "name": "rust-build-fix",
                        "description": "Repair the build and verify tests.",
                        "intent_tags": ["Coding"],
                        "domain_tags": ["rust"],
                        "preferred_tools": ["run_command", "read_file"],
                        "constraints": ["Do not claim success before reading the results."],
                        "lifecycle": {
                            "status": "reviewed",
                            "reviewed_by": "ops-team",
                            "review_notes": ["Validated in staging."],
                            "requires_operator_review": true
                        },
                        "steps": [
                            {
                                "title": "Run tests",
                                "instruction": "Run cargo test first.",
                                "tool": "run_command",
                                "arguments": { "command": "cargo", "args": ["test"] },
                                "verification": "Check exit_code is 0."
                            }
                        ]
                    }
                }),
                &SessionId::new(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(validation["validation"]["executable"], true);
        assert_eq!(validation["validation"]["can_save"], true);
        assert_eq!(validation["validation"]["can_approve"], false);
        assert_eq!(
            validation["validation"]["approval_stage"],
            "ready-for-approval"
        );
        assert_eq!(
            validation["validation"]["generated_script"]["language"],
            "bash"
        );

        let result = save_tool
            .execute(
                json!({
                    "skill": {
                        "name": "rust-build-fix",
                        "description": "Repair the build and verify tests.",
                        "intent_tags": ["Coding"],
                        "domain_tags": ["rust"],
                        "preferred_tools": ["run_command", "read_file"],
                        "constraints": ["Do not claim success before reading the results."],
                        "lifecycle": {
                            "status": "reviewed",
                            "reviewed_by": "ops-team",
                            "review_notes": ["Validated in staging."],
                            "requires_operator_review": true
                        },
                        "steps": [
                            {
                                "title": "Run tests",
                                "instruction": "Run cargo test first.",
                                "tool": "run_command",
                                "arguments": { "command": "cargo", "args": ["test"] },
                                "verification": "Check exit_code is 0."
                            }
                        ]
                    }
                }),
                &SessionId::new(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(result["saved"], true);
        assert_eq!(result["validation"]["executable"], true);
        assert_eq!(result["validation"]["approval_stage"], "ready-for-approval");

        let listed = list_tool
            .execute(json!({}), &SessionId::new(), None)
            .await
            .unwrap();
        assert_eq!(listed["count"], 1);
        assert_eq!(listed["skills"][0]["name"], "rust-build-fix");
        assert_eq!(listed["skills"][0]["status"], "reviewed");
        assert_eq!(listed["skills"][0]["version"], 1);

        let loaded = read_tool
            .execute(json!({ "name": "rust-build-fix" }), &SessionId::new(), None)
            .await
            .unwrap();
        assert_eq!(loaded["skill"]["steps"][0]["tool"], "run_command");
        assert_eq!(loaded["skill"]["lifecycle"]["usage_count"], 1);
        assert_eq!(loaded["skill"]["lifecycle"]["reviewed_by"], "ops-team");

        let filtered = list_tool
            .execute(json!({ "status": "reviewed" }), &SessionId::new(), None)
            .await
            .unwrap();
        assert_eq!(filtered["count"], 1);
    }

    #[tokio::test]
    async fn validate_skill_reports_missing_review_boundaries() {
        let store = SkillStore::new(temp_workspace().join("skills"));
        let validate_tool = ValidateSkillTool::new(store);

        let report = validate_tool
            .execute(
                json!({
                    "skill": {
                        "name": "dangerous-shell-fix",
                        "description": "Run a shell command without review metadata.",
                        "steps": [
                            {
                                "title": "Run command",
                                "instruction": "Run cargo fix.",
                                "tool": "run_command",
                                "arguments": { "command": "cargo", "args": ["fix"] }
                            }
                        ]
                    }
                }),
                &SessionId::new(),
                None,
            )
            .await
            .unwrap();

        assert_eq!(report["validation"]["can_save"], false);
        assert_eq!(report["validation"]["requires_operator_review"], true);
        assert_eq!(report["validation"]["approval_stage"], "needs-fixes");
        assert!(
            report["validation"]["missing_verification_steps"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == "Run command")
        );
    }

    #[tokio::test]
    async fn synthesize_skill_script_returns_preview_and_checklist() {
        let store = SkillStore::new(temp_workspace().join("skills"));
        let tool = SynthesizeSkillScriptTool::new(store);

        let result = tool
            .execute(
                json!({
                    "skill": {
                        "name": "rust-build-fix",
                        "description": "Repair the build and verify tests.",
                        "constraints": ["Stay inside the workspace."],
                        "steps": [
                            {
                                "title": "Run tests",
                                "instruction": "Run cargo test first.",
                                "tool": "run_command",
                                "arguments": { "command": "cargo", "args": ["test"] },
                                "verification": "Check exit_code is 0."
                            }
                        ]
                    }
                }),
                &SessionId::new(),
                None,
            )
            .await
            .unwrap();

        assert_eq!(result["generated"], true);
        assert_eq!(result["script"]["language"], "bash");
        assert!(
            result["script"]["content"]
                .as_str()
                .unwrap()
                .contains("cargo")
        );
        assert_eq!(result["approval_stage"], "awaiting-review");
        assert!(
            result["approval_checklist"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value.as_str().unwrap().contains("generated script preview"))
        );
    }

    #[tokio::test]
    async fn execute_skill_stages_runs_and_persists_journal() {
        let workspace = temp_workspace();
        let store = SkillStore::new(workspace.join("skills"));
        let save_tool = SaveSkillTool::new(store.clone());
        let exec_tool = ExecuteSkillStagesTool::new(
            store.clone(),
            workspace.clone(),
            tool_test_sandbox_config(),
        );

        save_tool
            .execute(
                json!({
                    "skill": {
                        "name": "echo-smoke",
                        "description": "Run a harmless command and record the checkpoint.",
                        "constraints": ["Stay inside the workspace."],
                        "steps": [
                            {
                                "title": "Echo",
                                "instruction": "Echo a short marker.",
                                "tool": "run_command",
                                "arguments": successful_command_args(),
                                "verification": "Check exit_code is 0.",
                                "rollback": "No rollback needed for this verification-only step.",
                                "checkpoints": ["capture the command output"]
                            }
                        ]
                    }
                }),
                &SessionId::new(),
                None,
            )
            .await
            .unwrap();

        let result = exec_tool
            .execute(
                json!({
                    "name": "echo-smoke",
                    "confirm": true
                }),
                &SessionId::new(),
                None,
            )
            .await
            .unwrap();

        assert_eq!(result["executed"], true);
        assert_eq!(result["status"], "completed");
        assert_eq!(result["journal"]["executed_steps"], 1);
        assert!(
            result["journal"]["steps"][0]["result"]["stdout"]
                .as_str()
                .unwrap()
                .to_lowercase()
                .contains("hello")
        );

        let stored = store.get("echo-smoke").unwrap();
        assert_eq!(stored.lifecycle.execution_count, 1);
        assert_eq!(
            stored.lifecycle.last_execution_status.as_deref(),
            Some("completed")
        );
        assert!(stored.lifecycle.last_checkpoint_at.is_some());
    }

    #[tokio::test]
    async fn execute_skill_stages_rolls_back_supported_write_steps_after_failure() {
        let workspace = temp_workspace();
        let store = SkillStore::new(workspace.join("skills"));
        let exec_tool =
            ExecuteSkillStagesTool::new(store, workspace.clone(), SandboxConfig::default());

        let result = exec_tool
            .execute(
                json!({
                    "skill": {
                        "name": "write-then-fail",
                        "description": "Write a file, then fail so rollback is exercised.",
                        "constraints": ["Stay inside the workspace."],
                        "steps": [
                            {
                                "title": "Write note",
                                "instruction": "Write a rollback test file.",
                                "tool": "write_file",
                                "arguments": { "path": "notes/rollback.txt", "content": "temp" },
                                "verification": "Check the file exists.",
                                "rollback": "Restore the previous file contents or remove the file if it was created during this run.",
                                "checkpoints": ["snapshot the file before writing"]
                            },
                            {
                                "title": "Fail fast",
                                "instruction": "Run a command that fails.",
                                "tool": "run_command",
                                "arguments": { "command": "definitely-not-a-real-command-ngenorca" },
                                "verification": "Expect the command to fail.",
                                "rollback": "Stop and review the prior state before retrying.",
                                "checkpoints": ["capture the failure"]
                            }
                        ]
                    },
                    "confirm": true,
                    "auto_rollback_on_failure": true
                }),
                &SessionId::new(),
                None,
            )
            .await
            .unwrap();

        assert_eq!(result["status"], "rolled_back");
        assert!(!workspace.join("notes/rollback.txt").exists());
        assert!(
            result["journal"]["steps"]
                .as_array()
                .unwrap()
                .iter()
                .any(|step| step["status"] == "rolled_back")
        );
    }

    #[tokio::test]
    async fn save_skill_rejects_invalid_artifact() {
        let store = SkillStore::new(temp_workspace().join("skills"));
        let save_tool = SaveSkillTool::new(store);

        let err = save_tool
            .execute(
                json!({
                    "skill": {
                        "name": "broken-skill",
                        "description": "",
                        "steps": []
                    }
                }),
                &SessionId::new(),
                None,
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("description") || err.to_string().contains("step"));
    }

    #[test]
    fn command_sandbox_policy_applies_configured_caps() {
        let workspace = temp_workspace();
        let mut sandbox = SandboxConfig::default();
        sandbox.policy.allow_network = true;
        sandbox.policy.allow_workspace_write = false;
        sandbox.policy.allow_child_processes = false;
        sandbox.policy.additional_read_paths = vec!["C:/tmp/ngenorca-read".into()];
        sandbox.policy.additional_write_paths = vec!["C:/tmp/ngenorca-write".into()];
        sandbox.policy.memory_limit_mb = 256;
        sandbox.policy.cpu_limit_seconds = 7;
        sandbox.policy.wall_time_limit_seconds = 5;

        let policy = command_sandbox_policy(20, &workspace, &sandbox);

        assert!(policy.allow_network);
        assert!(!policy.allow_spawn);
        assert_eq!(policy.memory_limit_bytes, 256 * 1024 * 1024);
        assert_eq!(policy.cpu_time_limit_secs, 7);
        assert_eq!(policy.wall_timeout_secs, 5);
        assert!(
            policy
                .allow_read_paths
                .contains(&workspace.to_string_lossy().to_string())
        );
        assert!(
            !policy
                .allow_write_paths
                .contains(&workspace.to_string_lossy().to_string())
        );
        assert!(
            policy
                .allow_write_paths
                .contains(&"C:/tmp/ngenorca-write".to_string())
        );
    }

    #[test]
    fn skill_artifact_schema_roundtrip() {
        let artifact = SkillArtifact {
            name: "deploy-checklist".into(),
            description: "Run deployment checks.".into(),
            intent_tags: vec!["Planning".into()],
            domain_tags: vec!["deployment".into()],
            preferred_tools: vec!["run_command".into()],
            constraints: vec![],
            steps: vec![AutomationStep {
                title: "Run smoke test".into(),
                instruction: "Run the smoke test command.".into(),
                step_id: Some("run-smoke-test".into()),
                tool: Some("run_command".into()),
                arguments: None,
                verification: Some("Confirm the smoke test passes.".into()),
                rollback: Some(
                    "If the smoke test changes state, revert the test fixture afterward.".into(),
                ),
                checkpoints: vec!["record smoke test result".into()],
                platform_hints: vec!["linux".into()],
                requires_confirmation: false,
            }],
            examples: vec![],
            lifecycle: ngenorca_plugin_sdk::SkillLifecycle {
                version: 2,
                status: SkillArtifactStatus::Approved,
                created_at: Some("2026-03-14T00:00:00Z".into()),
                updated_at: Some("2026-03-14T01:00:00Z".into()),
                last_used_at: None,
                usage_count: 3,
                execution_count: 4,
                last_executed_at: Some("2026-03-14T02:00:00Z".into()),
                last_checkpoint_at: Some("2026-03-14T02:01:00Z".into()),
                last_execution_status: Some("completed".into()),
                requires_operator_review: false,
                reviewed_by: Some("ops-team".into()),
                review_notes: vec!["Approved for repeated deployment checks.".into()],
            },
        };

        let value = serde_json::to_value(&artifact).unwrap();
        let back: SkillArtifact = serde_json::from_value(value).unwrap();
        assert_eq!(back.name, "deploy-checklist");
        assert_eq!(back.steps.len(), 1);
        assert_eq!(back.lifecycle.status, SkillArtifactStatus::Approved);
        assert_eq!(back.lifecycle.usage_count, 3);
        assert_eq!(back.lifecycle.execution_count, 4);
        assert_eq!(
            back.lifecycle.last_execution_status.as_deref(),
            Some("completed")
        );
    }
}

#[derive(Clone)]
struct SkillTool {
    store: SkillStore,
}

impl SkillTool {
    fn new(store: SkillStore) -> Self {
        Self { store }
    }
}

struct ListSkillsTool {
    base: SkillTool,
}

impl ListSkillsTool {
    fn new(store: SkillStore) -> Self {
        Self {
            base: SkillTool::new(store),
        }
    }
}

#[async_trait]
impl AgentTool for ListSkillsTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "list_skills".into(),
            description: "List reusable skill and automation artifacts stored for future tasks."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": ["draft", "reviewed", "approved", "deprecated"]
                    },
                    "requires_operator_review": { "type": "boolean" }
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
        let status_filter = arguments
            .get("status")
            .and_then(|value| value.as_str())
            .map(parse_skill_status)
            .transpose()?;
        let review_filter = arguments
            .get("requires_operator_review")
            .and_then(|value| value.as_bool());

        let mut skills = self.base.store.list()?;
        if let Some(status) = status_filter {
            skills.retain(|skill| skill.status == status);
        }
        if let Some(requires_operator_review) = review_filter {
            skills.retain(|skill| skill.requires_operator_review == requires_operator_review);
        }

        Ok(json!({
            "count": skills.len(),
            "skills": skills,
        }))
    }
}

struct ReadSkillTool {
    base: SkillTool,
}

impl ReadSkillTool {
    fn new(store: SkillStore) -> Self {
        Self {
            base: SkillTool::new(store),
        }
    }
}

#[async_trait]
impl AgentTool for ReadSkillTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_skill".into(),
            description: "Read a stored skill or automation artifact, including ordered steps and verification guidance.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" }
                },
                "required": ["name"],
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
        let name = arguments
            .get("name")
            .and_then(|value| value.as_str())
            .ok_or_else(|| Error::Gateway("Missing 'name'".into()))?;

        let skill = self.base.store.get(name)?;
        Ok(json!({ "skill": skill }))
    }
}

struct SaveSkillTool {
    base: SkillTool,
}

struct ValidateSkillTool {
    base: SkillTool,
}

struct SynthesizeSkillScriptTool {
    base: SkillTool,
}

struct ExecuteSkillStagesTool {
    base: SkillTool,
    workspace: WorkspaceTool,
    sandbox: SandboxConfig,
}

#[derive(Debug, Clone)]
struct PreparedWriteRollback {
    step_number: usize,
    title: String,
    target: PathBuf,
    target_display: String,
    backup_artifact: Option<String>,
    existed_before: bool,
}

impl SaveSkillTool {
    fn new(store: SkillStore) -> Self {
        Self {
            base: SkillTool::new(store),
        }
    }
}

impl ValidateSkillTool {
    fn new(store: SkillStore) -> Self {
        Self {
            base: SkillTool::new(store),
        }
    }
}

impl SynthesizeSkillScriptTool {
    fn new(store: SkillStore) -> Self {
        Self {
            base: SkillTool::new(store),
        }
    }
}

impl ExecuteSkillStagesTool {
    fn new(store: SkillStore, workspace_root: PathBuf, sandbox: SandboxConfig) -> Self {
        Self {
            base: SkillTool::new(store),
            workspace: WorkspaceTool::new(workspace_root),
            sandbox,
        }
    }

    fn load_skill(&self, arguments: &serde_json::Value) -> Result<(SkillArtifact, Option<String>)> {
        if let Some(name) = arguments.get("name").and_then(|value| value.as_str()) {
            return Ok((self.base.store.get(name)?, Some(name.to_string())));
        }

        let skill_value = arguments
            .get("skill")
            .cloned()
            .ok_or_else(|| Error::Gateway("Missing 'name' or 'skill'".into()))?;
        let skill: SkillArtifact = serde_json::from_value(skill_value)?;
        Ok((skill, None))
    }

    fn checkpoint_record(
        label: impl Into<String>,
        phase: impl Into<String>,
        detail: Option<String>,
    ) -> SkillCheckpointRecord {
        SkillCheckpointRecord {
            label: label.into(),
            phase: phase.into(),
            recorded_at: chrono::Utc::now().to_rfc3339(),
            detail,
        }
    }

    fn write_file_step(
        &self,
        journal_id: &str,
        step_number: usize,
        step: &AutomationStep,
    ) -> Result<(serde_json::Value, Option<PreparedWriteRollback>)> {
        let arguments = step
            .arguments
            .as_ref()
            .and_then(|value| value.as_object())
            .ok_or_else(|| {
                Error::Gateway(format!(
                    "Step {} is missing write_file arguments",
                    step_number
                ))
            })?;
        let path = arguments
            .get("path")
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                Error::Gateway(format!("Step {} is missing write_file.path", step_number))
            })?;
        let content = arguments
            .get("content")
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                Error::Gateway(format!(
                    "Step {} is missing write_file.content",
                    step_number
                ))
            })?;
        let append = arguments
            .get("append")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let create_dirs = arguments
            .get("create_dirs")
            .and_then(|value| value.as_bool())
            .unwrap_or(true);

        let file_path = self.workspace.resolve(path)?;
        if create_dirs && let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::Gateway(format!(
                    "Cannot create parent directories '{}': {e}",
                    parent.display()
                ))
            })?;
        }

        let existed_before = file_path.exists();
        let backup_artifact = if existed_before {
            let backup_dir = self.base.store.execution_root().join("backups");
            std::fs::create_dir_all(&backup_dir)?;
            let artifact = format!("{}-step-{}.bak", journal_id, step_number);
            let backup_path = backup_dir.join(&artifact);
            let bytes = std::fs::read(&file_path).map_err(|e| {
                Error::Gateway(format!(
                    "Cannot snapshot file '{}': {e}",
                    file_path.display()
                ))
            })?;
            std::fs::write(&backup_path, bytes).map_err(|e| {
                Error::Gateway(format!(
                    "Cannot persist rollback snapshot '{}': {e}",
                    backup_path.display()
                ))
            })?;
            Some(format!("backups/{}", artifact))
        } else {
            None
        };

        if append {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&file_path)
                .map_err(|e| {
                    Error::Gateway(format!("Cannot open file '{}': {e}", file_path.display()))
                })?;
            file.write_all(content.as_bytes()).map_err(|e| {
                Error::Gateway(format!("Cannot append file '{}': {e}", file_path.display()))
            })?;
        } else {
            std::fs::write(&file_path, content).map_err(|e| {
                Error::Gateway(format!("Cannot write file '{}': {e}", file_path.display()))
            })?;
        }

        Ok((
            json!({
                "path": maybe_relativize(&file_path, &self.workspace.workspace_root),
                "bytes_written": content.len(),
                "append": append,
            }),
            Some(PreparedWriteRollback {
                step_number,
                title: step.title.clone(),
                target_display: maybe_relativize(&file_path, &self.workspace.workspace_root),
                target: file_path,
                backup_artifact,
                existed_before,
            }),
        ))
    }

    fn execute_write_rollback(
        &self,
        prepared: &PreparedWriteRollback,
    ) -> Result<SkillRollbackPlanEntry> {
        if let Some(artifact) = prepared.backup_artifact.as_deref() {
            let backup_path = self.base.store.execution_root().join(artifact);
            let bytes = std::fs::read(&backup_path).map_err(|e| {
                Error::Gateway(format!(
                    "Cannot read rollback snapshot '{}': {e}",
                    backup_path.display()
                ))
            })?;
            if let Some(parent) = prepared.target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&prepared.target, bytes).map_err(|e| {
                Error::Gateway(format!(
                    "Cannot restore file '{}': {e}",
                    prepared.target.display()
                ))
            })?;
        } else if !prepared.existed_before && prepared.target.exists() {
            std::fs::remove_file(&prepared.target).map_err(|e| {
                Error::Gateway(format!(
                    "Cannot remove file '{}': {e}",
                    prepared.target.display()
                ))
            })?;
        }

        Ok(SkillRollbackPlanEntry {
            step_number: prepared.step_number,
            step_id: None,
            title: prepared.title.clone(),
            strategy: "restore-file-snapshot".into(),
            available: true,
            instruction: if prepared.backup_artifact.is_some() {
                "Restored the captured file snapshot.".into()
            } else {
                "Removed the file created during staged execution.".into()
            },
            target: Some(prepared.target_display.clone()),
            recovery_artifact: prepared.backup_artifact.clone(),
        })
    }
}

fn skill_lifecycle_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "version": { "type": "integer", "minimum": 1 },
            "status": {
                "type": "string",
                "enum": ["draft", "reviewed", "approved", "deprecated"]
            },
            "created_at": { "type": ["string", "null"] },
            "updated_at": { "type": ["string", "null"] },
            "last_used_at": { "type": ["string", "null"] },
            "usage_count": { "type": "integer", "minimum": 0 },
            "execution_count": { "type": "integer", "minimum": 0 },
            "last_executed_at": { "type": ["string", "null"] },
            "last_checkpoint_at": { "type": ["string", "null"] },
            "last_execution_status": { "type": ["string", "null"] },
            "requires_operator_review": { "type": "boolean" },
            "reviewed_by": { "type": ["string", "null"] },
            "review_notes": { "type": "array", "items": { "type": "string" } }
        },
        "additionalProperties": false
    })
}

fn automation_step_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "title": { "type": "string" },
            "instruction": { "type": "string" },
            "step_id": { "type": ["string", "null"] },
            "tool": { "type": ["string", "null"] },
            "arguments": {},
            "verification": { "type": ["string", "null"] },
            "rollback": { "type": ["string", "null"] },
            "checkpoints": { "type": "array", "items": { "type": "string" } },
            "platform_hints": { "type": "array", "items": { "type": "string" } },
            "requires_confirmation": { "type": "boolean" }
        },
        "required": ["title", "instruction"],
        "additionalProperties": false
    })
}

fn skill_artifact_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "description": { "type": "string" },
            "intent_tags": { "type": "array", "items": { "type": "string" } },
            "domain_tags": { "type": "array", "items": { "type": "string" } },
            "preferred_tools": { "type": "array", "items": { "type": "string" } },
            "constraints": { "type": "array", "items": { "type": "string" } },
            "examples": { "type": "array", "items": { "type": "string" } },
            "lifecycle": skill_lifecycle_schema(),
            "steps": {
                "type": "array",
                "items": automation_step_schema()
            }
        },
        "required": ["name", "description", "steps"],
        "additionalProperties": false
    })
}

#[async_trait]
impl AgentTool for SaveSkillTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "save_skill".into(),
            description: "Persist a reusable skill or automation artifact with structured steps, tools, and verification notes.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "skill": skill_artifact_schema()
                },
                "required": ["skill"],
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
        let skill_value = arguments
            .get("skill")
            .cloned()
            .ok_or_else(|| Error::Gateway("Missing 'skill'".into()))?;
        let skill: SkillArtifact = serde_json::from_value(skill_value)?;
        let validation = validate_skill_artifact(&skill);
        let summary = self.base.store.save(&skill)?;

        Ok(json!({
            "saved": true,
            "skill": summary,
            "validation": validation,
            "storage_root": self.base.store.root().to_string_lossy().to_string(),
        }))
    }
}

#[async_trait]
impl AgentTool for ValidateSkillTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "validate_skill".into(),
            description: "Validate a skill or automation artifact, especially executable steps that need review boundaries and verification guidance.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "skill": skill_artifact_schema()
                },
                "required": ["skill"],
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
        let skill_value = arguments
            .get("skill")
            .cloned()
            .ok_or_else(|| Error::Gateway("Missing 'skill'".into()))?;
        let skill: SkillArtifact = serde_json::from_value(skill_value)?;
        let validation = validate_skill_artifact(&skill);

        Ok(json!({
            "valid": validation.can_save,
            "validation": validation,
            "storage_root": self.base.store.root().to_string_lossy().to_string(),
        }))
    }
}

#[async_trait]
impl AgentTool for SynthesizeSkillScriptTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "synthesize_skill_script".into(),
            description: "Generate a script preview and staged approval checklist for an executable skill or automation recipe.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "skill": skill_artifact_schema()
                },
                "required": ["skill"],
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
        let skill_value = arguments
            .get("skill")
            .cloned()
            .ok_or_else(|| Error::Gateway("Missing 'skill'".into()))?;
        let skill: SkillArtifact = serde_json::from_value(skill_value)?;
        let validation = validate_skill_artifact(&skill);
        let script = synthesize_skill_script(&skill);

        Ok(json!({
            "generated": script.is_some(),
            "script": script,
            "approval_stage": validation.approval_stage,
            "approval_checklist": validation.approval_checklist,
            "storage_root": self.base.store.root().to_string_lossy().to_string(),
        }))
    }
}

#[async_trait]
impl AgentTool for ExecuteSkillStagesTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "execute_skill_stages".into(),
            description: "Execute the generated skill preview as staged steps, persist checkpoints, and prepare rollback artifacts for supported write steps.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Stored skill name to execute." },
                    "skill": {
                        "type": "object",
                        "description": "Inline skill artifact when the skill is not yet stored. Provide either 'name' or 'skill'."
                    },
                    "through_step": { "type": "integer", "minimum": 1 },
                    "cwd": { "type": "string", "description": "Optional relative path inside the workspace." },
                    "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 300 },
                    "confirm": { "type": "boolean", "description": "Required for operator-reviewed or confirmation-gated executable skills." },
                    "auto_rollback_on_failure": { "type": "boolean" },
                    "stop_on_failure": { "type": "boolean" }
                },
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
        let (skill, stored_name) = self.load_skill(&arguments)?;
        let validation = validate_skill_artifact(&skill);
        if !validation.executable {
            return Err(Error::Gateway(
                "Skill does not contain any directly executable preview steps".into(),
            ));
        }

        let confirm = arguments
            .get("confirm")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        if (validation.requires_operator_review
            || skill.steps.iter().any(|step| step.requires_confirmation))
            && !confirm
        {
            return Err(Error::Gateway(
                "Explicit 'confirm': true is required before staged execution".into(),
            ));
        }

        let through_step = arguments
            .get("through_step")
            .and_then(|value| value.as_u64())
            .map(|value| value as usize)
            .unwrap_or(skill.steps.len());
        if through_step == 0 || through_step > skill.steps.len() {
            return Err(Error::Gateway(format!(
                "through_step must be between 1 and {}",
                skill.steps.len()
            )));
        }

        let target_steps = &skill.steps[..through_step];
        let missing_rollbacks = target_steps
            .iter()
            .filter(|step| {
                step.tool
                    .as_deref()
                    .is_some_and(|tool| matches!(tool, "run_command" | "write_file"))
                    && step
                        .rollback
                        .as_deref()
                        .map(str::trim)
                        .unwrap_or_default()
                        .is_empty()
            })
            .map(|step| step.title.clone())
            .collect::<Vec<_>>();
        if !missing_rollbacks.is_empty() {
            return Err(Error::Gateway(format!(
                "Targeted staged execution is blocked until rollback guidance exists for: {}",
                missing_rollbacks.join(", ")
            )));
        }

        let missing_checkpoints = target_steps
            .iter()
            .filter(|step| {
                step.tool
                    .as_deref()
                    .is_some_and(|tool| matches!(tool, "run_command" | "write_file"))
                    && step.checkpoints.is_empty()
            })
            .map(|step| step.title.clone())
            .collect::<Vec<_>>();
        if !missing_checkpoints.is_empty() {
            return Err(Error::Gateway(format!(
                "Targeted staged execution is blocked until checkpoints exist for: {}",
                missing_checkpoints.join(", ")
            )));
        }

        let timeout_secs = arguments
            .get("timeout_secs")
            .and_then(|value| value.as_u64())
            .unwrap_or(DEFAULT_COMMAND_TIMEOUT_SECS);
        let stop_on_failure = arguments
            .get("stop_on_failure")
            .and_then(|value| value.as_bool())
            .unwrap_or(true);
        let auto_rollback_on_failure = arguments
            .get("auto_rollback_on_failure")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let cwd = arguments
            .get("cwd")
            .and_then(|value| value.as_str())
            .map(|value| self.workspace.resolve(value))
            .transpose()?
            .unwrap_or_else(|| self.workspace.workspace_root.clone());

        let journal_id = new_skill_execution_journal_id(&skill.name);
        let started_at = chrono::Utc::now().to_rfc3339();
        let mut journal = SkillExecutionJournal {
            journal_id: journal_id.clone(),
            skill_name: skill.name.clone(),
            skill_version: skill.lifecycle.version,
            status: SkillExecutionStatus::Running,
            started_at: started_at.clone(),
            updated_at: started_at,
            workspace_root: self.workspace.workspace_root.to_string_lossy().to_string(),
            cwd: cwd.to_string_lossy().to_string(),
            through_step,
            executed_steps: 0,
            checkpoint_count: 0,
            rollback_ready_steps: 0,
            steps: Vec::new(),
            rollback_plan: synthesize_rollback_plan(&skill, through_step),
            script_preview: synthesize_skill_script(&skill),
        };
        let mut last_checkpoint_at = None;
        self.base.store.save_execution_journal(&journal)?;

        let mut prepared_rollbacks = Vec::<PreparedWriteRollback>::new();

        for (index, step) in target_steps.iter().enumerate() {
            let step_number = index + 1;
            let mut record = SkillExecutionStepRecord {
                step_number,
                step_id: step.step_id.clone(),
                title: step.title.clone(),
                tool: step.tool.clone(),
                status: SkillExecutionStatus::Running,
                checkpoints: step
                    .checkpoints
                    .iter()
                    .map(|checkpoint| Self::checkpoint_record(checkpoint.clone(), "planned", None))
                    .collect(),
                verification: step.verification.clone(),
                rollback: journal
                    .rollback_plan
                    .iter()
                    .find(|entry| entry.step_number == step_number)
                    .cloned(),
                result: None,
                error: None,
            };
            journal.checkpoint_count += record.checkpoints.len();
            if let Some(last) = record.checkpoints.last() {
                last_checkpoint_at = Some(last.recorded_at.clone());
            }

            let outcome = match step.tool.as_deref() {
                Some("run_command") => {
                    let step_args = step
                        .arguments
                        .as_ref()
                        .and_then(|value| value.as_object())
                        .ok_or_else(|| {
                            Error::Gateway(format!(
                                "Step {} is missing run_command arguments",
                                step_number
                            ))
                        })?;
                    let command = step_args
                        .get("command")
                        .and_then(|value| value.as_str())
                        .ok_or_else(|| {
                            Error::Gateway(format!("Step {} is missing command", step_number))
                        })?;
                    let args = step_args
                        .get("args")
                        .and_then(|value| value.as_array())
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|value| value.as_str().map(ToOwned::to_owned))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();

                    match run_command_capture(
                        command,
                        &args,
                        &cwd,
                        timeout_secs,
                        &self.workspace.workspace_root,
                        &self.sandbox,
                    )
                    .await
                    {
                        Ok(result) => {
                            let success = result
                                .get("success")
                                .and_then(|value| value.as_bool())
                                .unwrap_or(false)
                                && !result
                                    .get("timed_out")
                                    .and_then(|value| value.as_bool())
                                    .unwrap_or(false);
                            record.result = Some(result.clone());
                            if success {
                                record.status = SkillExecutionStatus::Completed;
                                Ok(())
                            } else {
                                record.status = SkillExecutionStatus::Failed;
                                record.error = Some(format!(
                                    "Command step '{}' failed or timed out",
                                    step.title
                                ));
                                Err(Error::Gateway(format!(
                                    "Command step '{}' failed or timed out",
                                    step.title
                                )))
                            }
                        }
                        Err(err) => {
                            let message = err.to_string();
                            record.result = Some(failed_command_result(
                                command,
                                &args,
                                &cwd,
                                &self.sandbox,
                                &self.workspace.workspace_root,
                                timeout_secs,
                                &message,
                            ));
                            record.status = SkillExecutionStatus::Failed;
                            record.error = Some(message.clone());
                            Err(Error::Gateway(message))
                        }
                    }
                }
                Some("write_file") => {
                    let (result, prepared) =
                        self.write_file_step(&journal_id, step_number, step)?;
                    record.result = Some(result);
                    record.status = SkillExecutionStatus::Completed;
                    if let Some(prepared) = prepared {
                        journal.rollback_ready_steps += 1;
                        if let Some(existing) = record.rollback.as_mut() {
                            existing.available = true;
                            existing.target = Some(prepared.target_display.clone());
                            existing.recovery_artifact = prepared.backup_artifact.clone();
                        }
                        prepared_rollbacks.push(prepared);
                    }
                    Ok(())
                }
                Some(_) => {
                    record.status = SkillExecutionStatus::Manual;
                    record.error = Some(
                        "This tool is not directly executable from staged skill automation yet."
                            .into(),
                    );
                    Ok(())
                }
                None => {
                    record.status = SkillExecutionStatus::Manual;
                    record.error = Some(
                        "This stage has no tool binding and still requires operator/manual execution."
                            .into(),
                    );
                    Ok(())
                }
            };

            if matches!(record.status, SkillExecutionStatus::Completed) {
                journal.executed_steps += 1;
            } else if outcome.is_err() {
                journal.status = SkillExecutionStatus::Failed;
            }

            journal.steps.push(record.clone());
            journal.updated_at = chrono::Utc::now().to_rfc3339();
            self.base.store.save_execution_journal(&journal)?;

            if outcome.is_err() && stop_on_failure {
                break;
            }
        }

        if journal.status == SkillExecutionStatus::Failed && auto_rollback_on_failure {
            let mut rollback_applied = Vec::new();
            for prepared in prepared_rollbacks.iter().rev() {
                let applied = self.execute_write_rollback(prepared)?;
                rollback_applied.push(applied.clone());
                journal.steps.push(SkillExecutionStepRecord {
                    step_number: prepared.step_number,
                    step_id: None,
                    title: format!("Rollback for {}", prepared.title),
                    tool: Some("write_file".into()),
                    status: SkillExecutionStatus::RolledBack,
                    checkpoints: vec![Self::checkpoint_record(
                        format!("rollback {}", prepared.target_display),
                        "rollback",
                        Some(applied.instruction.clone()),
                    )],
                    verification: None,
                    rollback: Some(applied.clone()),
                    result: Some(json!({ "rolled_back": true, "target": prepared.target_display })),
                    error: None,
                });
                journal.rollback_plan.iter_mut().for_each(|entry| {
                    if entry.step_number == prepared.step_number {
                        entry.available = true;
                        entry.target = Some(prepared.target_display.clone());
                        entry.recovery_artifact = prepared.backup_artifact.clone();
                    }
                });
            }
            if !rollback_applied.is_empty() {
                journal.status = SkillExecutionStatus::RolledBack;
                journal.updated_at = chrono::Utc::now().to_rfc3339();
                if let Some(last) = journal
                    .steps
                    .last()
                    .and_then(|step| step.checkpoints.last())
                {
                    last_checkpoint_at = Some(last.recorded_at.clone());
                }
                self.base.store.save_execution_journal(&journal)?;
            }
        }

        if journal.status == SkillExecutionStatus::Running {
            journal.status = SkillExecutionStatus::Completed;
        }
        journal.updated_at = chrono::Utc::now().to_rfc3339();
        let journal_path = self.base.store.save_execution_journal(&journal)?;

        if let Some(skill_name) = stored_name.as_deref() {
            self.base.store.record_execution_result(
                skill_name,
                journal.status,
                last_checkpoint_at.clone(),
            )?;
        }

        Ok(json!({
            "executed": matches!(journal.status, SkillExecutionStatus::Completed | SkillExecutionStatus::RolledBack),
            "status": skill_execution_status_label(journal.status),
            "validation": validation,
            "journal": journal,
            "journal_path": journal_path.to_string_lossy().to_string(),
            "last_checkpoint_at": last_checkpoint_at,
        }))
    }
}

fn parse_skill_status(raw: &str) -> Result<SkillArtifactStatus> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "draft" => Ok(SkillArtifactStatus::Draft),
        "reviewed" => Ok(SkillArtifactStatus::Reviewed),
        "approved" => Ok(SkillArtifactStatus::Approved),
        "deprecated" => Ok(SkillArtifactStatus::Deprecated),
        other => Err(Error::Gateway(format!("Unknown skill status: {other}"))),
    }
}
