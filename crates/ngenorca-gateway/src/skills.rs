//! Persistent skill and automation artifact storage.

use ngenorca_core::{Error, Result};
use ngenorca_plugin_sdk::{SkillArtifact, SkillArtifactStatus, SkillArtifactSummary};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkillScriptPreview {
    pub language: String,
    pub entrypoint: String,
    pub generated_steps: usize,
    pub content: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkillExecutionStagePreview {
    pub step_number: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    pub execution_kind: String,
    pub requires_confirmation: bool,
    #[serde(default)]
    pub checkpoints: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkillRollbackPlanEntry {
    pub step_number: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    pub title: String,
    pub strategy: String,
    pub available: bool,
    pub instruction: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_artifact: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillExecutionStatus {
    Planned,
    Running,
    Completed,
    Failed,
    Manual,
    Skipped,
    RolledBack,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkillCheckpointRecord {
    pub label: String,
    pub phase: String,
    pub recorded_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkillExecutionStepRecord {
    pub step_number: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    pub status: SkillExecutionStatus,
    #[serde(default)]
    pub checkpoints: Vec<SkillCheckpointRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback: Option<SkillRollbackPlanEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkillExecutionJournal {
    pub journal_id: String,
    pub skill_name: String,
    pub skill_version: u32,
    pub status: SkillExecutionStatus,
    pub started_at: String,
    pub updated_at: String,
    pub workspace_root: String,
    pub cwd: String,
    pub through_step: usize,
    pub executed_steps: usize,
    pub checkpoint_count: usize,
    pub rollback_ready_steps: usize,
    #[serde(default)]
    pub steps: Vec<SkillExecutionStepRecord>,
    #[serde(default)]
    pub rollback_plan: Vec<SkillRollbackPlanEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_preview: Option<SkillScriptPreview>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkillValidationReport {
    pub executable: bool,
    #[serde(default)]
    pub risky_tools: Vec<String>,
    #[serde(default)]
    pub missing_verification_steps: Vec<String>,
    #[serde(default)]
    pub missing_rollback_steps: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    pub requires_operator_review: bool,
    pub can_save: bool,
    pub can_approve: bool,
    pub review_boundary: String,
    pub approval_stage: String,
    #[serde(default)]
    pub approval_checklist: Vec<String>,
    #[serde(default)]
    pub staged_execution: Vec<SkillExecutionStagePreview>,
    #[serde(default)]
    pub rollback_plan: Vec<SkillRollbackPlanEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_script: Option<SkillScriptPreview>,
}

pub fn validate_skill_artifact(skill: &SkillArtifact) -> SkillValidationReport {
    let mut risky_tools = Vec::new();
    let mut missing_verification_steps = Vec::new();
    let mut missing_rollback_steps = Vec::new();

    for step in &skill.steps {
        let Some(tool) = step.tool.as_deref() else {
            continue;
        };

        if is_risky_tool(tool) && !risky_tools.iter().any(|existing| existing == tool) {
            risky_tools.push(tool.to_string());
        }

        if is_executable_tool(tool)
            && step
                .verification
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .is_empty()
        {
            missing_verification_steps.push(step.title.clone());
        }

        if is_state_mutating_tool(tool)
            && step
                .rollback
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .is_empty()
        {
            missing_rollback_steps.push(step.title.clone());
        }
    }

    let executable = !risky_tools.is_empty();
    let mut warnings = Vec::new();
    if executable && skill.constraints.is_empty() {
        warnings
            .push("Executable automations should declare at least one explicit constraint.".into());
    }
    if !missing_verification_steps.is_empty() {
        warnings.push(
            "Executable steps should include explicit verification guidance before reuse or approval."
                .into(),
        );
    }
    if !missing_rollback_steps.is_empty() {
        warnings.push(
            "State-mutating executable steps should include rollback guidance before staged execution or approval."
                .into(),
        );
    }
    if skill.steps.iter().any(|step| {
        step.tool.as_deref().is_some_and(is_executable_tool) && step.checkpoints.is_empty()
    }) {
        warnings.push(
            "Executable steps should define checkpoints so staged execution can expose visible progress."
                .into(),
        );
    }

    let requires_operator_review = skill.lifecycle.requires_operator_review || executable;
    let can_save = !skill.steps.is_empty()
        && (if executable {
            !skill.constraints.is_empty() && missing_verification_steps.is_empty()
        } else {
            true
        });
    let can_approve = can_save
        && !requires_operator_review
        && skill
            .lifecycle
            .reviewed_by
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| !value.is_empty())
        && !skill.lifecycle.review_notes.is_empty();
    let generated_script = synthesize_skill_script(skill);
    let staged_execution = synthesize_staged_execution(skill);
    let rollback_plan = synthesize_rollback_plan(skill, skill.steps.len());
    let approval_stage = approval_stage(skill, requires_operator_review, can_save, can_approve);
    let approval_checklist = approval_checklist(
        skill,
        &missing_verification_steps,
        &missing_rollback_steps,
        requires_operator_review,
        generated_script.is_some(),
        !staged_execution.is_empty(),
        !rollback_plan.is_empty(),
    );

    let review_boundary = if executable {
        format!(
            "This skill uses executable or side-effecting tools ({}). Treat it as an operator-reviewed automation recipe and confirm verification steps before approval or reuse.",
            risky_tools.join(", ")
        )
    } else {
        "This skill is advisory-only and can be reviewed as a reusable planning/reference artifact."
            .into()
    };

    SkillValidationReport {
        executable,
        risky_tools,
        missing_verification_steps,
        missing_rollback_steps,
        warnings,
        requires_operator_review,
        can_save,
        can_approve,
        review_boundary,
        approval_stage,
        approval_checklist,
        staged_execution,
        rollback_plan,
        generated_script,
    }
}

pub fn synthesize_skill_script(skill: &SkillArtifact) -> Option<SkillScriptPreview> {
    let executable_steps = skill
        .steps
        .iter()
        .filter(|step| step.tool.as_deref().is_some_and(is_executable_tool))
        .count();
    if executable_steps == 0 {
        return None;
    }

    let mut lines = vec![
        "#!/usr/bin/env bash".into(),
        "set -euo pipefail".into(),
        format!("# Skill: {}", skill.name),
        format!("# Description: {}", skill.description),
    ];

    for constraint in &skill.constraints {
        lines.push(format!("# Constraint: {}", constraint));
    }

    for (index, step) in skill.steps.iter().enumerate() {
        lines.push(String::new());
        lines.push(format!("# Step {}: {}", index + 1, step.title));
        if let Some(step_id) = step.step_id.as_deref() {
            lines.push(format!("# Step ID: {}", step_id));
        }
        lines.push(format!(
            "echo {}",
            shell_quote(&format!("Step {}: {}", index + 1, step.title))
        ));
        if step.requires_confirmation {
            lines.push("# Confirmation required before executing this step.".into());
        }

        match step.tool.as_deref() {
            Some("run_command") => {
                if let Some(command) = render_run_command(step.arguments.as_ref()) {
                    lines.push(command);
                } else {
                    lines.push(format!("# {}", step.instruction));
                }
            }
            Some("write_file") => {
                if let Some(command) = render_write_file(step.arguments.as_ref()) {
                    lines.push(command);
                } else {
                    lines.push(format!("# {}", step.instruction));
                }
            }
            Some(tool) => {
                lines.push(format!(
                    "# Tool {} is referenced here: {}",
                    tool, step.instruction
                ));
            }
            None => lines.push(format!("# {}", step.instruction)),
        }

        if let Some(verification) = step
            .verification
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            lines.push(format!("# Verify: {}", verification));
        }
        for checkpoint in &step.checkpoints {
            lines.push(format!("# Checkpoint: {}", checkpoint));
        }
        if let Some(rollback) = step
            .rollback
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            lines.push(format!("# Rollback: {}", rollback));
        }
        if !step.platform_hints.is_empty() {
            lines.push(format!("# Platforms: {}", step.platform_hints.join(", ")));
        }
    }

    Some(SkillScriptPreview {
        language: "bash".into(),
        entrypoint: "bash generated-skill.sh".into(),
        generated_steps: executable_steps,
        content: lines.join("\n"),
    })
}

pub fn synthesize_staged_execution(skill: &SkillArtifact) -> Vec<SkillExecutionStagePreview> {
    skill
        .steps
        .iter()
        .enumerate()
        .filter(|(_, step)| step.tool.as_deref().is_some_and(is_executable_tool))
        .map(|(index, step)| SkillExecutionStagePreview {
            step_number: index + 1,
            step_id: step.step_id.clone(),
            title: step.title.clone(),
            tool: step.tool.clone(),
            execution_kind: if step.requires_confirmation {
                "confirmed-executable".into()
            } else {
                "executable".into()
            },
            requires_confirmation: step.requires_confirmation,
            checkpoints: step.checkpoints.clone(),
            verification: step
                .verification
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
        })
        .collect()
}

pub fn synthesize_rollback_plan(
    skill: &SkillArtifact,
    through_step: usize,
) -> Vec<SkillRollbackPlanEntry> {
    skill
        .steps
        .iter()
        .enumerate()
        .take(through_step)
        .rev()
        .filter_map(|(index, step)| {
            let tool = step.tool.as_deref()?;
            if !is_executable_tool(tool) {
                return None;
            }

            let rollback = step.rollback.as_deref().map(str::trim).unwrap_or_default();
            let target = if tool == "write_file" {
                extract_write_file_target(step.arguments.as_ref())
            } else {
                None
            };
            let available = match tool {
                "write_file" => target.is_some(),
                _ => !rollback.is_empty(),
            };
            let strategy = match tool {
                "write_file" if target.is_some() => "restore-file-snapshot",
                "run_command" => "operator-guided",
                _ => "manual",
            };
            let instruction = if !rollback.is_empty() {
                rollback.to_string()
            } else if tool == "write_file" {
                "Restore the last captured file snapshot before re-running later stages.".into()
            } else {
                format!(
                    "Review step '{}' manually before re-running the automation.",
                    step.title
                )
            };

            Some(SkillRollbackPlanEntry {
                step_number: index + 1,
                step_id: step.step_id.clone(),
                title: step.title.clone(),
                strategy: strategy.into(),
                available,
                instruction,
                target,
                recovery_artifact: None,
            })
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct SkillStore {
    root: PathBuf,
}

impl SkillStore {
    pub fn new(root: PathBuf) -> Self {
        std::fs::create_dir_all(&root).ok();
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn execution_root(&self) -> PathBuf {
        let root = self.root.join("executions");
        std::fs::create_dir_all(&root).ok();
        root
    }

    pub fn list(&self) -> Result<Vec<SkillArtifactSummary>> {
        let mut skills = Vec::new();

        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }

            let skill = self.read_path(&path)?;
            skills.push(Self::summarize(&skill));
        }

        skills.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(skills)
    }

    pub fn get(&self, name: &str) -> Result<SkillArtifact> {
        let path = self.path_for(name);
        if !path.exists() {
            return Err(Error::NotFound(format!("Skill not found: {name}")));
        }

        let mut skill = self.read_path(&path)?;
        skill.lifecycle.usage_count += 1;
        skill.lifecycle.last_used_at = Some(timestamp_now());
        self.write_path(&path, &skill)?;
        Ok(skill)
    }

    pub fn save(&self, skill: &SkillArtifact) -> Result<SkillArtifactSummary> {
        Self::validate(skill)?;
        let path = self.path_for(&skill.name);
        let now = timestamp_now();
        let existing = if path.exists() {
            Some(self.read_path(&path)?)
        } else {
            None
        };

        let mut stored = skill.clone();
        stored.lifecycle.version = existing
            .as_ref()
            .map(|current| current.lifecycle.version.saturating_add(1))
            .unwrap_or(1);
        stored.lifecycle.created_at = existing
            .as_ref()
            .and_then(|current| current.lifecycle.created_at.clone())
            .or_else(|| Some(now.clone()));
        stored.lifecycle.updated_at = Some(now);
        stored.lifecycle.last_used_at = existing
            .as_ref()
            .and_then(|current| current.lifecycle.last_used_at.clone())
            .or_else(|| stored.lifecycle.last_used_at.clone());
        stored.lifecycle.usage_count = existing
            .as_ref()
            .map(|current| current.lifecycle.usage_count)
            .unwrap_or(stored.lifecycle.usage_count);
        stored.lifecycle.execution_count = existing
            .as_ref()
            .map(|current| current.lifecycle.execution_count)
            .unwrap_or(stored.lifecycle.execution_count);
        stored.lifecycle.last_executed_at = existing
            .as_ref()
            .and_then(|current| current.lifecycle.last_executed_at.clone())
            .or_else(|| stored.lifecycle.last_executed_at.clone());
        stored.lifecycle.last_checkpoint_at = existing
            .as_ref()
            .and_then(|current| current.lifecycle.last_checkpoint_at.clone())
            .or_else(|| stored.lifecycle.last_checkpoint_at.clone());
        stored.lifecycle.last_execution_status = existing
            .as_ref()
            .and_then(|current| current.lifecycle.last_execution_status.clone())
            .or_else(|| stored.lifecycle.last_execution_status.clone());

        self.write_path(&path, &stored)?;
        Ok(Self::summarize(&stored))
    }

    pub fn save_execution_journal(&self, journal: &SkillExecutionJournal) -> Result<PathBuf> {
        let path = self
            .execution_root()
            .join(format!("{}.json", journal.journal_id));
        let content = serde_json::to_string_pretty(journal)?;
        std::fs::write(&path, content)?;
        Ok(path)
    }

    pub fn load_execution_journal(&self, journal_id: &str) -> Result<SkillExecutionJournal> {
        let path = self.execution_root().join(format!("{}.json", journal_id));
        let content = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&content)?)
    }

    pub fn record_execution_result(
        &self,
        skill_name: &str,
        status: SkillExecutionStatus,
        last_checkpoint_at: Option<String>,
    ) -> Result<()> {
        let path = self.path_for(skill_name);
        if !path.exists() {
            return Ok(());
        }

        let mut skill = self.read_path(&path)?;
        skill.lifecycle.execution_count = skill.lifecycle.execution_count.saturating_add(1);
        skill.lifecycle.last_executed_at = Some(timestamp_now());
        skill.lifecycle.last_execution_status = Some(skill_execution_status_label(status).into());
        if let Some(last_checkpoint_at) = last_checkpoint_at {
            skill.lifecycle.last_checkpoint_at = Some(last_checkpoint_at);
        }
        self.write_path(&path, &skill)
    }

    fn summarize(skill: &SkillArtifact) -> SkillArtifactSummary {
        SkillArtifactSummary {
            name: skill.name.clone(),
            description: skill.description.clone(),
            intent_tags: skill.intent_tags.clone(),
            domain_tags: skill.domain_tags.clone(),
            step_count: skill.steps.len(),
            version: skill.lifecycle.version,
            status: skill.lifecycle.status.clone(),
            usage_count: skill.lifecycle.usage_count,
            requires_operator_review: skill.lifecycle.requires_operator_review,
        }
    }

    fn validate(skill: &SkillArtifact) -> Result<()> {
        if skill.name.trim().is_empty() {
            return Err(Error::Gateway("Skill name cannot be empty".into()));
        }
        if skill.description.trim().is_empty() {
            return Err(Error::Gateway("Skill description cannot be empty".into()));
        }
        if skill.steps.is_empty() {
            return Err(Error::Gateway(
                "Skill must include at least one automation step".into(),
            ));
        }

        let validation = validate_skill_artifact(skill);
        if validation.executable && skill.constraints.is_empty() {
            return Err(Error::Gateway(
                "Executable skills must declare at least one constraint".into(),
            ));
        }
        if !validation.missing_verification_steps.is_empty() {
            return Err(Error::Gateway(format!(
                "Executable skill steps must include verification guidance: {}",
                validation.missing_verification_steps.join(", ")
            )));
        }

        if matches!(
            skill.lifecycle.status,
            SkillArtifactStatus::Reviewed | SkillArtifactStatus::Approved
        ) {
            if skill
                .lifecycle
                .reviewed_by
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .is_empty()
            {
                return Err(Error::Gateway(
                    "Reviewed or approved skills must record `reviewed_by`".into(),
                ));
            }

            if skill.lifecycle.review_notes.is_empty() {
                return Err(Error::Gateway(
                    "Reviewed or approved skills must include at least one review note".into(),
                ));
            }
        }

        if skill.lifecycle.status == SkillArtifactStatus::Approved
            && skill.lifecycle.requires_operator_review
        {
            return Err(Error::Gateway(
                "Approved skills cannot still require operator review".into(),
            ));
        }

        for (index, step) in skill.steps.iter().enumerate() {
            if step.title.trim().is_empty() {
                return Err(Error::Gateway(format!(
                    "Skill step {} must have a title",
                    index + 1
                )));
            }
            if step.instruction.trim().is_empty() {
                return Err(Error::Gateway(format!(
                    "Skill step {} must have an instruction",
                    index + 1
                )));
            }
        }

        Ok(())
    }

    fn read_path(&self, path: &Path) -> Result<SkillArtifact> {
        let content = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&content)?)
    }

    fn write_path(&self, path: &Path, skill: &SkillArtifact) -> Result<()> {
        let content = serde_json::to_string_pretty(skill)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    fn path_for(&self, name: &str) -> PathBuf {
        self.root.join(format!("{}.json", slugify(name)))
    }
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;

    for ch in value.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            slug.push(lower);
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }

    slug.trim_matches('-').to_string()
}

fn timestamp_now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn is_executable_tool(tool: &str) -> bool {
    matches!(tool, "run_command" | "write_file")
}

fn is_state_mutating_tool(tool: &str) -> bool {
    matches!(tool, "run_command" | "write_file")
}

fn is_risky_tool(tool: &str) -> bool {
    matches!(
        tool,
        "run_command" | "write_file" | "fetch_url" | "web_search"
    )
}

fn approval_stage(
    skill: &SkillArtifact,
    requires_operator_review: bool,
    can_save: bool,
    can_approve: bool,
) -> String {
    if skill.lifecycle.status == SkillArtifactStatus::Approved && can_approve {
        return "approved".into();
    }
    if !can_save {
        return "needs-fixes".into();
    }
    if requires_operator_review {
        if skill
            .lifecycle
            .reviewed_by
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
            || skill.lifecycle.review_notes.is_empty()
        {
            return "awaiting-review".into();
        }
        return "ready-for-approval".into();
    }
    if can_approve {
        "ready-for-approval".into()
    } else {
        "draft".into()
    }
}

fn approval_checklist(
    skill: &SkillArtifact,
    missing_verification_steps: &[String],
    missing_rollback_steps: &[String],
    requires_operator_review: bool,
    has_generated_script: bool,
    has_staged_execution: bool,
    has_rollback_plan: bool,
) -> Vec<String> {
    let mut checklist = Vec::new();

    if skill.constraints.is_empty()
        && skill
            .steps
            .iter()
            .any(|step| step.tool.as_deref().is_some_and(is_executable_tool))
    {
        checklist.push("Add at least one explicit execution constraint.".into());
    }

    for step in missing_verification_steps {
        checklist.push(format!("Add verification guidance for step '{}'.", step));
    }

    for step in missing_rollback_steps {
        checklist.push(format!(
            "Add rollback guidance for state-mutating step '{}'.",
            step
        ));
    }

    if has_generated_script {
        checklist.push("Inspect the generated script preview and confirm it matches the intended tool arguments.".into());
    }
    if has_staged_execution {
        checklist.push("Review the staged execution preview and confirm the checkpoint order matches the intended rollout.".into());
    }
    if has_rollback_plan {
        checklist.push(
            "Review the rollback plan in reverse order before any staged execution begins.".into(),
        );
    }

    if requires_operator_review {
        if skill
            .lifecycle
            .reviewed_by
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
        {
            checklist.push("Assign an operator reviewer in lifecycle.reviewed_by.".into());
        }
        if skill.lifecycle.review_notes.is_empty() {
            checklist.push(
                "Record review_notes that justify why this automation is safe to reuse.".into(),
            );
        }
    }

    if checklist.is_empty() {
        checklist.push("No open approval blockers.".into());
    }

    checklist
}

fn render_run_command(arguments: Option<&Value>) -> Option<String> {
    let arguments = arguments?.as_object()?;
    let command = arguments.get("command")?.as_str()?;
    let mut parts = vec![shell_quote(command)];

    if let Some(args) = arguments.get("args").and_then(|value| value.as_array()) {
        for arg in args {
            if let Some(arg) = arg.as_str() {
                parts.push(shell_quote(arg));
            }
        }
    }

    Some(parts.join(" "))
}

fn render_write_file(arguments: Option<&Value>) -> Option<String> {
    let arguments = arguments?.as_object()?;
    let path = arguments.get("path")?.as_str()?;
    let content = arguments.get("content").and_then(|value| value.as_str())?;
    let heredoc = "NGENORCA_EOF";

    Some(format!(
        "mkdir -p \"$(dirname {})\"\ncat > {} <<'{heredoc}'\n{}\n{heredoc}",
        shell_quote(path),
        shell_quote(path),
        content
    ))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace("'", "'\"'\"'"))
}

fn extract_write_file_target(arguments: Option<&Value>) -> Option<String> {
    arguments?
        .as_object()?
        .get("path")?
        .as_str()
        .map(ToOwned::to_owned)
}

pub fn new_skill_execution_journal_id(skill_name: &str) -> String {
    format!(
        "{}-{}-{}",
        slugify(skill_name),
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    )
}

pub fn skill_execution_status_label(status: SkillExecutionStatus) -> &'static str {
    match status {
        SkillExecutionStatus::Planned => "planned",
        SkillExecutionStatus::Running => "running",
        SkillExecutionStatus::Completed => "completed",
        SkillExecutionStatus::Failed => "failed",
        SkillExecutionStatus::Manual => "manual",
        SkillExecutionStatus::Skipped => "skipped",
        SkillExecutionStatus::RolledBack => "rolled_back",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ngenorca_plugin_sdk::AutomationStep;

    fn temp_store() -> SkillStore {
        let dir = std::env::temp_dir().join(format!(
            "ngenorca_skill_store_{}_{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        SkillStore::new(dir)
    }

    fn sample_skill() -> SkillArtifact {
        SkillArtifact {
            name: "Rust Build Fix".into(),
            description: "Fix build issues and verify tests.".into(),
            intent_tags: vec!["Coding".into()],
            domain_tags: vec!["rust".into()],
            preferred_tools: vec!["run_command".into()],
            constraints: vec!["Do not claim success without verification.".into()],
            steps: vec![AutomationStep {
                title: "Run tests".into(),
                instruction: "Run the relevant test command first.".into(),
                step_id: Some("run-tests".into()),
                tool: Some("run_command".into()),
                arguments: Some(serde_json::json!({"command": "cargo", "args": ["test"]})),
                verification: Some("Check exit_code is 0.".into()),
                rollback: Some("No rollback needed for this verification step.".into()),
                checkpoints: vec!["capture the failing test output".into()],
                platform_hints: vec!["windows".into(), "linux".into()],
                requires_confirmation: false,
            }],
            examples: vec![],
            lifecycle: Default::default(),
        }
    }

    #[test]
    fn save_and_get_skill_roundtrip() {
        let store = temp_store();
        let summary = store.save(&sample_skill()).unwrap();
        assert_eq!(summary.name, "Rust Build Fix");
        assert_eq!(summary.version, 1);
        assert_eq!(summary.status, SkillArtifactStatus::Draft);
        assert!(summary.requires_operator_review);

        let loaded = store.get("Rust Build Fix").unwrap();
        assert_eq!(loaded.steps.len(), 1);
        assert_eq!(loaded.domain_tags[0], "rust");
        assert_eq!(loaded.lifecycle.usage_count, 1);
        assert!(loaded.lifecycle.last_used_at.is_some());
    }

    #[test]
    fn list_skills_returns_summaries() {
        let store = temp_store();
        store.save(&sample_skill()).unwrap();

        let skills = store.list().unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].step_count, 1);
        assert_eq!(skills[0].version, 1);
    }

    #[test]
    fn validate_rejects_missing_steps() {
        let store = temp_store();
        let mut skill = sample_skill();
        skill.steps.clear();
        assert!(store.save(&skill).is_err());
    }

    #[test]
    fn save_bumps_version_and_preserves_created_at() {
        let store = temp_store();
        let first = store.save(&sample_skill()).unwrap();
        assert_eq!(first.version, 1);

        let mut updated = sample_skill();
        updated.description = "Fix build issues, rerun tests, and summarize regressions.".into();
        let second = store.save(&updated).unwrap();
        assert_eq!(second.version, 2);

        let path = store.path_for("Rust Build Fix");
        let raw = store.read_path(&path).unwrap();
        assert_eq!(raw.lifecycle.version, 2);
        assert!(raw.lifecycle.created_at.is_some());
        assert!(raw.lifecycle.updated_at.is_some());
    }

    #[test]
    fn validate_rejects_approved_skill_without_review_metadata() {
        let store = temp_store();
        let mut skill = sample_skill();
        skill.lifecycle.status = SkillArtifactStatus::Approved;
        skill.lifecycle.requires_operator_review = false;

        let err = store.save(&skill).unwrap_err();
        assert!(err.to_string().contains("reviewed_by") || err.to_string().contains("review note"));
    }

    #[test]
    fn validation_report_marks_executable_skills_for_review() {
        let skill = sample_skill();
        let report = validate_skill_artifact(&skill);

        assert!(report.executable);
        assert!(report.requires_operator_review);
        assert_eq!(report.risky_tools, vec!["run_command"]);
        assert!(report.can_save);
        assert!(!report.can_approve);
        assert!(report.missing_rollback_steps.is_empty());
        assert_eq!(report.approval_stage, "awaiting-review");
        assert_eq!(report.staged_execution.len(), 1);
        assert_eq!(report.rollback_plan.len(), 1);
        assert!(report.generated_script.is_some());
        assert!(
            report
                .approval_checklist
                .iter()
                .any(|item| item.contains("generated script preview"))
        );
        assert!(
            report
                .approval_checklist
                .iter()
                .any(|item| item.contains("staged execution preview"))
        );
        assert!(
            report
                .approval_checklist
                .iter()
                .any(|item| item.contains("rollback plan"))
        );
        assert!(
            report
                .generated_script
                .as_ref()
                .unwrap()
                .content
                .contains("cargo")
        );
        assert!(
            report
                .generated_script
                .as_ref()
                .unwrap()
                .content
                .contains("Rollback")
        );
    }

    #[test]
    fn validation_report_marks_reviewed_skill_ready_for_approval() {
        let mut skill = sample_skill();
        skill.lifecycle.reviewed_by = Some("ops-team".into());
        skill.lifecycle.review_notes = vec!["Dry-run reviewed.".into()];

        let report = validate_skill_artifact(&skill);
        assert_eq!(report.approval_stage, "ready-for-approval");
        assert!(
            report
                .approval_checklist
                .iter()
                .any(|item| item.contains("generated script preview"))
        );
    }

    #[test]
    fn validate_rejects_executable_skill_without_constraints_or_verification() {
        let store = temp_store();
        let mut skill = sample_skill();
        skill.constraints.clear();
        skill.steps[0].verification = None;
        skill.steps[0].rollback = None;

        let err = store.save(&skill).unwrap_err();
        assert!(
            err.to_string().contains("constraint")
                || err.to_string().contains("verification guidance")
        );
    }

    #[test]
    fn synthesize_rollback_plan_tracks_write_file_targets_in_reverse_order() {
        let mut skill = sample_skill();
        skill.steps.push(AutomationStep {
            title: "Patch config".into(),
            instruction: "Write a temporary config override.".into(),
            step_id: Some("patch-config".into()),
            tool: Some("write_file".into()),
            arguments: Some(serde_json::json!({
                "path": "config/local.toml",
                "content": "mode = 'test'"
            })),
            verification: Some("Confirm the override file exists.".into()),
            rollback: Some("Restore the previous file contents after the dry run.".into()),
            checkpoints: vec!["snapshot config before writing".into()],
            platform_hints: vec![],
            requires_confirmation: true,
        });

        let rollback_plan = synthesize_rollback_plan(&skill, skill.steps.len());
        assert_eq!(rollback_plan.len(), 2);
        assert_eq!(rollback_plan[0].title, "Patch config");
        assert_eq!(
            rollback_plan[0].target.as_deref(),
            Some("config/local.toml")
        );
        assert_eq!(rollback_plan[0].strategy, "restore-file-snapshot");
    }

    #[test]
    fn execution_journal_and_lifecycle_metadata_are_persisted() {
        let store = temp_store();
        store.save(&sample_skill()).unwrap();

        let journal = SkillExecutionJournal {
            journal_id: new_skill_execution_journal_id("Rust Build Fix"),
            skill_name: "Rust Build Fix".into(),
            skill_version: 1,
            status: SkillExecutionStatus::Completed,
            started_at: timestamp_now(),
            updated_at: timestamp_now(),
            workspace_root: "workspace".into(),
            cwd: "workspace".into(),
            through_step: 1,
            executed_steps: 1,
            checkpoint_count: 1,
            rollback_ready_steps: 0,
            steps: vec![SkillExecutionStepRecord {
                step_number: 1,
                step_id: Some("run-tests".into()),
                title: "Run tests".into(),
                tool: Some("run_command".into()),
                status: SkillExecutionStatus::Completed,
                checkpoints: vec![SkillCheckpointRecord {
                    label: "capture the failing test output".into(),
                    phase: "captured".into(),
                    recorded_at: timestamp_now(),
                    detail: Some("stdout captured".into()),
                }],
                verification: Some("Check exit_code is 0.".into()),
                rollback: None,
                result: Some(serde_json::json!({ "success": true })),
                error: None,
            }],
            rollback_plan: vec![],
            script_preview: synthesize_skill_script(&sample_skill()),
        };

        let path = store.save_execution_journal(&journal).unwrap();
        assert!(path.exists());
        let loaded = store.load_execution_journal(&journal.journal_id).unwrap();
        assert_eq!(loaded.executed_steps, 1);

        let checkpoint_at = loaded.steps[0].checkpoints[0].recorded_at.clone();
        store
            .record_execution_result(
                "Rust Build Fix",
                SkillExecutionStatus::Completed,
                Some(checkpoint_at.clone()),
            )
            .unwrap();

        let stored = store.get("Rust Build Fix").unwrap();
        assert_eq!(stored.lifecycle.execution_count, 1);
        assert_eq!(
            stored.lifecycle.last_execution_status.as_deref(),
            Some("completed")
        );
        assert_eq!(
            stored.lifecycle.last_checkpoint_at.as_deref(),
            Some(checkpoint_at.as_str())
        );
    }

    #[test]
    fn slugify_normalizes_skill_names() {
        assert_eq!(slugify("Rust Build Fix"), "rust-build-fix");
        assert_eq!(slugify("  Deploy_Checklist v2 "), "deploy-checklist-v2");
    }
}
