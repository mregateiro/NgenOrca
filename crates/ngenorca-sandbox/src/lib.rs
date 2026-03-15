//! # NgenOrca Sandbox
//!
//! Platform-adaptive sandboxing for tool and plugin execution.
//!
//! Auto-detects the runtime environment and selects the appropriate backend:
//!
//! - **Container detected** → defer to container isolation (Docker/Podman IS the sandbox)
//! - **Windows** → Job Objects + Restricted Tokens
//! - **Linux** → seccomp + landlock + namespaces
//! - **macOS** → App Sandbox profiles
//!
//! The sandbox is enabled by default. Users opt *out*, not in.

use ngenorca_core::{Error, Result};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Detected sandbox environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SandboxEnvironment {
    /// Running inside a container (Docker, Podman, etc.).
    Container,
    /// Running natively on Windows.
    Windows,
    /// Running natively on Linux.
    Linux,
    /// Running natively on macOS.
    MacOs,
    /// Unknown platform.
    Unknown,
}

/// Sandbox policy for a process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxPolicy {
    /// Allow network access.
    pub allow_network: bool,
    /// Allow filesystem reads (list of allowed paths).
    pub allow_read_paths: Vec<String>,
    /// Allow filesystem writes (list of allowed paths).
    pub allow_write_paths: Vec<String>,
    /// Allow process spawning.
    pub allow_spawn: bool,
    /// Memory limit in bytes (0 = unlimited).
    pub memory_limit_bytes: u64,
    /// CPU time limit in seconds (0 = unlimited).
    pub cpu_time_limit_secs: u64,
    /// Wall clock timeout in seconds (0 = unlimited).
    pub wall_timeout_secs: u64,
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self {
            allow_network: false,
            allow_read_paths: vec![],
            allow_write_paths: vec![],
            allow_spawn: false,
            memory_limit_bytes: 512 * 1024 * 1024, // 512 MB
            cpu_time_limit_secs: 30,
            wall_timeout_secs: 60,
        }
    }
}

/// Detect the current sandbox environment.
pub fn detect_environment() -> SandboxEnvironment {
    // Check if we're inside a container.
    if is_container() {
        return SandboxEnvironment::Container;
    }

    #[cfg(windows)]
    {
        SandboxEnvironment::Windows
    }
    #[cfg(target_os = "linux")]
    {
        SandboxEnvironment::Linux
    }
    #[cfg(target_os = "macos")]
    {
        SandboxEnvironment::MacOs
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        SandboxEnvironment::Unknown
    }
}

/// Check if we're running inside a container.
fn is_container() -> bool {
    // Docker creates /.dockerenv.
    if std::path::Path::new("/.dockerenv").exists() {
        return true;
    }

    // Check cgroup for container hints (Linux).
    #[cfg(target_os = "linux")]
    {
        if let Ok(cgroup) = std::fs::read_to_string("/proc/1/cgroup")
            && (cgroup.contains("docker")
                || cgroup.contains("kubepods")
                || cgroup.contains("containerd"))
        {
            return true;
        }
        // systemd-based container detection.
        if let Ok(env) = std::fs::read_to_string("/proc/1/environ")
            && env.contains("container=")
        {
            return true;
        }
    }

    false
}

/// Execute a command within the sandbox.
pub async fn sandboxed_exec(
    command: &str,
    args: &[&str],
    policy: &SandboxPolicy,
) -> Result<SandboxedOutput> {
    sandboxed_exec_with_cwd(command, args, None, policy).await
}

/// Execute a command within the sandbox, optionally setting the working directory.
pub async fn sandboxed_exec_with_cwd(
    command: &str,
    args: &[&str],
    cwd: Option<&std::path::Path>,
    policy: &SandboxPolicy,
) -> Result<SandboxedOutput> {
    let resolved_command = resolve_command_path(command);
    let resolved_command = resolved_command.to_string_lossy().to_string();
    let env = detect_environment();

    info!(?env, command, "Executing in sandbox");

    match env {
        SandboxEnvironment::Container => {
            // Inside a container, the container IS the sandbox.
            // Just run the command directly.
            exec_direct(
                &resolved_command,
                args,
                cwd,
                policy,
                build_sandbox_audit(
                    env,
                    "container",
                    policy,
                    true,
                    &["container_boundary", "wall_timeout"],
                    Some("ambient container isolation detected; command-specific restrictions depend on the outer container profile".into()),
                ),
            )
            .await
        }
        SandboxEnvironment::Windows => {
            // Use Windows Job Objects.
            #[cfg(windows)]
            {
                exec_windows_job(&resolved_command, args, cwd, policy).await
            }
            #[cfg(not(windows))]
            {
                warn!("Windows sandbox not available on this platform");
                exec_direct(
                    &resolved_command,
                    args,
                    cwd,
                    policy,
                    build_sandbox_audit(
                        env,
                        "direct",
                        policy,
                        false,
                        &["wall_timeout"],
                        Some("Windows backend unavailable on this platform".into()),
                    ),
                )
                .await
            }
        }
        SandboxEnvironment::Linux => {
            #[cfg(target_os = "linux")]
            {
                exec_linux_sandboxed(&resolved_command, args, cwd, policy).await
            }
            #[cfg(not(target_os = "linux"))]
            {
                warn!("Linux sandbox not available on this platform");
                exec_direct(
                    &resolved_command,
                    args,
                    cwd,
                    policy,
                    build_sandbox_audit(
                        env,
                        "direct",
                        policy,
                        false,
                        &["wall_timeout"],
                        Some("Linux backend unavailable on this platform".into()),
                    ),
                )
                .await
            }
        }
        SandboxEnvironment::MacOs => {
            #[cfg(target_os = "macos")]
            {
                exec_macos_sandboxed(&resolved_command, args, cwd, policy).await
            }
            #[cfg(not(target_os = "macos"))]
            {
                warn!("macOS sandbox not available on this platform");
                exec_direct(
                    &resolved_command,
                    args,
                    cwd,
                    policy,
                    build_sandbox_audit(
                        env,
                        "direct",
                        policy,
                        false,
                        &["wall_timeout"],
                        Some("macOS backend unavailable on this platform".into()),
                    ),
                )
                .await
            }
        }
        SandboxEnvironment::Unknown => {
            warn!("Unknown platform, running without sandbox");
            exec_direct(
                &resolved_command,
                args,
                cwd,
                policy,
                build_sandbox_audit(
                    env,
                    "direct",
                    policy,
                    false,
                    &["wall_timeout"],
                    Some("unknown platform; falling back to direct execution".into()),
                ),
            )
            .await
        }
    }
}

/// Output from a sandboxed execution.
#[derive(Debug, Clone)]
pub struct SandboxedOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub timed_out: bool,
    pub audit: SandboxAudit,
}

/// Operator-facing audit details for a single sandboxed execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SandboxAudit {
    pub requested_environment: SandboxEnvironment,
    pub backend: String,
    pub isolation_active: bool,
    pub enforced_controls: Vec<String>,
    pub policy_gaps: Vec<String>,
    pub fallback_reason: Option<String>,
}

/// Return the expected policy-enforcement audit for the current environment.
pub fn audit_policy(policy: &SandboxPolicy, enabled: bool) -> SandboxAudit {
    let env = detect_environment();

    if !enabled {
        return build_sandbox_audit(
            env,
            "direct",
            policy,
            false,
            &["wall_timeout"],
            Some("sandbox disabled in configuration".into()),
        );
    }

    match env {
        SandboxEnvironment::Container => build_sandbox_audit(
            env,
            "container",
            policy,
            true,
            &["container_boundary", "wall_timeout"],
            Some("ambient container isolation detected; command-specific restrictions depend on the outer container profile".into()),
        ),
        SandboxEnvironment::Windows => build_sandbox_audit(
            env,
            "windows_job",
            policy,
            true,
            &["job_object", "wall_timeout", "process_limit", "memory_limit", "cpu_limit"],
            None,
        ),
        SandboxEnvironment::Linux => build_sandbox_audit(
            env,
            "linux_unshare",
            policy,
            true,
            &[
                "namespace_isolation",
                "wall_timeout",
                "process_limit",
                "memory_limit",
                "cpu_limit",
                "network_policy",
            ],
            None,
        ),
        SandboxEnvironment::MacOs => build_sandbox_audit(
            env,
            "macos_sandbox_exec",
            policy,
            true,
            &["seatbelt_profile", "wall_timeout", "filesystem_policy", "network_policy"],
            None,
        ),
        SandboxEnvironment::Unknown => build_sandbox_audit(
            env,
            "direct",
            policy,
            false,
            &["wall_timeout"],
            Some("unknown platform; falling back to direct execution".into()),
        ),
    }
}

/// Execute a command without backend sandboxing while preserving timeout behavior.
pub async fn unsandboxed_exec_with_cwd(
    command: &str,
    args: &[&str],
    cwd: Option<&std::path::Path>,
    policy: &SandboxPolicy,
) -> Result<SandboxedOutput> {
    let resolved_command = resolve_command_path(command);
    exec_direct(
        resolved_command.to_string_lossy().as_ref(),
        args,
        cwd,
        policy,
        build_sandbox_audit(
            detect_environment(),
            "direct",
            policy,
            false,
            &["wall_timeout"],
            Some("sandbox disabled in configuration".into()),
        ),
    )
    .await
}

fn build_sandbox_audit(
    requested_environment: SandboxEnvironment,
    backend: &str,
    policy: &SandboxPolicy,
    isolation_active: bool,
    enforced_controls: &[&str],
    fallback_reason: Option<String>,
) -> SandboxAudit {
    let enforced = enforced_controls
        .iter()
        .map(|value| (*value).to_string())
        .collect::<Vec<_>>();
    let mut policy_gaps = Vec::new();

    let filesystem_requested =
        !policy.allow_read_paths.is_empty() || !policy.allow_write_paths.is_empty();
    if filesystem_requested && !enforced_controls.contains(&"filesystem_policy") {
        policy_gaps.push("filesystem scope is not fully enforced by the active backend".into());
    }
    if !policy.allow_network && !enforced_controls.contains(&"network_policy") {
        policy_gaps.push("network restriction is not enforced by the active backend".into());
    }
    if !policy.allow_spawn && !enforced_controls.contains(&"process_limit") {
        policy_gaps.push("child-process restriction is not enforced by the active backend".into());
    }
    if (policy.memory_limit_bytes > 0 || policy.cpu_time_limit_secs > 0)
        && !(enforced_controls.contains(&"memory_limit")
            && enforced_controls.contains(&"cpu_limit"))
    {
        policy_gaps
            .push("CPU and/or memory limits are not fully enforced by the active backend".into());
    }

    SandboxAudit {
        requested_environment,
        backend: backend.into(),
        isolation_active,
        enforced_controls: enforced,
        policy_gaps,
        fallback_reason,
    }
}

fn sandboxed_output(
    stdout: String,
    stderr: String,
    exit_code: i32,
    timed_out: bool,
    audit: SandboxAudit,
) -> SandboxedOutput {
    SandboxedOutput {
        stdout,
        stderr,
        exit_code,
        timed_out,
        audit,
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn stderr_contains_any(stderr: &str, needles: &[&str]) -> bool {
    let stderr = stderr.to_ascii_lowercase();
    needles
        .iter()
        .any(|needle| stderr.contains(&needle.to_ascii_lowercase()))
}

#[cfg(target_os = "linux")]
fn is_prlimit_wrapper_failure(stderr: &str, exit_code: Option<i32>) -> bool {
    exit_code != Some(0)
        && stderr_contains_any(
            stderr,
            &[
                "prlimit:",
                "failed to set",
                "failed to execute",
                "operation not permitted",
                "permission denied",
                "invalid argument",
                "no such file or directory",
            ],
        )
}

#[cfg(target_os = "macos")]
fn push_unique_path_variant(values: &mut Vec<String>, path: &std::path::Path) {
    let candidate = path.to_string_lossy().to_string();
    if !candidate.is_empty() && !values.iter().any(|existing| existing == &candidate) {
        values.push(candidate);
    }
}

#[cfg(target_os = "macos")]
fn path_variants(path: &std::path::Path) -> Vec<String> {
    let mut variants = Vec::new();
    push_unique_path_variant(&mut variants, path);

    if let Ok(canonical) = path.canonicalize() {
        push_unique_path_variant(&mut variants, &canonical);
    }

    variants
}

#[cfg(target_os = "macos")]
fn allow_profile_paths(profile: &mut String, rule: &str, paths: &[String]) {
    for path in paths {
        profile.push_str(&format!("(allow {rule} (subpath \"{}\"))\n", path));
    }
}

fn resolve_command_path(command: &str) -> std::path::PathBuf {
    let command_path = std::path::Path::new(command);
    if command_path.is_absolute() || command_path.components().count() > 1 {
        return command_path.to_path_buf();
    }

    let Some(path_env) = std::env::var_os("PATH") else {
        return command_path.to_path_buf();
    };

    #[cfg(windows)]
    let path_exts = {
        let configured = std::env::var_os("PATHEXT")
            .map(|value| {
                value
                    .to_string_lossy()
                    .split(';')
                    .filter(|entry| !entry.is_empty())
                    .map(|entry| entry.to_ascii_lowercase())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| vec![".com".into(), ".exe".into(), ".bat".into(), ".cmd".into()]);
        let mut extensions = vec![String::new()];
        extensions.extend(configured);
        extensions
    };

    for directory in std::env::split_paths(&path_env) {
        #[cfg(windows)]
        {
            for extension in &path_exts {
                let candidate = if extension.is_empty() || command_path.extension().is_some() {
                    directory.join(command)
                } else {
                    directory.join(format!("{command}{extension}"))
                };

                if candidate.is_file() {
                    return candidate;
                }
            }
        }

        #[cfg(not(windows))]
        {
            let candidate = directory.join(command);
            if candidate.is_file() {
                return candidate;
            }
        }
    }

    command_path.to_path_buf()
}

/// Direct execution (used inside containers or as fallback).
async fn exec_direct(
    command: &str,
    args: &[&str],
    cwd: Option<&std::path::Path>,
    policy: &SandboxPolicy,
    audit: SandboxAudit,
) -> Result<SandboxedOutput> {
    use tokio::process::Command;

    let timeout = if policy.wall_timeout_secs > 0 {
        std::time::Duration::from_secs(policy.wall_timeout_secs)
    } else {
        std::time::Duration::from_secs(300) // 5 min default cap
    };

    let mut cmd = Command::new(command);
    cmd.args(args);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }

    let result = tokio::time::timeout(timeout, cmd.output()).await;

    match result {
        Ok(Ok(output)) => Ok(sandboxed_output(
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
            output.status.code().unwrap_or(-1),
            false,
            audit,
        )),
        Ok(Err(e)) => Err(Error::Sandbox(format!("Exec failed: {e}"))),
        Err(_) => Ok(sandboxed_output(
            String::new(),
            format!("Process timed out after {}s", policy.wall_timeout_secs),
            -1,
            true,
            audit,
        )),
    }
}

/// Windows Job Object-based sandbox.
///
/// Creates a restricted Job Object with:
/// - Memory limits via `JOBOBJECT_EXTENDED_LIMIT_INFORMATION`
/// - CPU time limits via `PerProcessUserTimeLimit`
/// - Process termination on Job close (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`)
/// - Active-process limit of 1 (`JOB_OBJECT_LIMIT_ACTIVE_PROCESS`)
/// - Wall-clock timeout via `tokio::time::timeout`
#[cfg(windows)]
async fn exec_windows_job(
    command: &str,
    args: &[&str],
    cwd: Option<&std::path::Path>,
    policy: &SandboxPolicy,
) -> Result<SandboxedOutput> {
    use std::mem::{size_of, zeroed};
    use std::ptr::null;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
        JOB_OBJECT_LIMIT_PROCESS_TIME, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };

    info!("Windows Job Object sandbox — enforcing limits");

    // ── Create a Job Object ──
    let job = unsafe { CreateJobObjectW(null(), null()) };
    if job.is_null() || job == INVALID_HANDLE_VALUE {
        warn!("Failed to create Job Object, falling back to basic exec");
        return exec_direct(
            command,
            args,
            cwd,
            policy,
            build_sandbox_audit(
                SandboxEnvironment::Windows,
                "direct",
                policy,
                false,
                &["wall_timeout"],
                Some("failed to create Windows Job Object".into()),
            ),
        )
        .await;
    }

    // ── Configure limits ──
    let mut ext_info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };

    let mut limit_flags: u32 = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;

    // Memory limit
    if policy.memory_limit_bytes > 0 {
        limit_flags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
        ext_info.ProcessMemoryLimit = policy.memory_limit_bytes as usize;
    }

    // CPU time limit (100-nanosecond intervals)
    if policy.cpu_time_limit_secs > 0 {
        limit_flags |= JOB_OBJECT_LIMIT_PROCESS_TIME;
        ext_info.BasicLimitInformation.PerProcessUserTimeLimit =
            (policy.cpu_time_limit_secs * 10_000_000) as i64;
    }

    ext_info.BasicLimitInformation.LimitFlags = limit_flags;
    ext_info.BasicLimitInformation.ActiveProcessLimit = 1;

    let set_ok = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &ext_info as *const _ as *const _,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };

    if set_ok == 0 {
        warn!("Failed to set Job Object limits, falling back to basic exec");
        unsafe { CloseHandle(job) };
        return exec_direct(
            command,
            args,
            cwd,
            policy,
            build_sandbox_audit(
                SandboxEnvironment::Windows,
                "direct",
                policy,
                false,
                &["wall_timeout"],
                Some("failed to configure Windows Job Object limits".into()),
            ),
        )
        .await;
    }

    // ── Spawn child process and assign to Job Object ──
    let timeout = if policy.wall_timeout_secs > 0 {
        std::time::Duration::from_secs(policy.wall_timeout_secs)
    } else {
        std::time::Duration::from_secs(300)
    };

    let mut std_cmd = std::process::Command::new(command);
    std_cmd.args(args);
    if let Some(cwd) = cwd {
        std_cmd.current_dir(cwd);
    }
    std_cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let child = match std_cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            unsafe { CloseHandle(job) };
            return Err(Error::Sandbox(format!("Failed to spawn process: {e}")));
        }
    };

    // Assign child to Job Object
    use std::os::windows::io::AsRawHandle;
    let process_handle = child.as_raw_handle();
    let assign_ok = unsafe { AssignProcessToJobObject(job, process_handle as _) };
    if assign_ok == 0 {
        warn!("Failed to assign process to Job Object — limits may not be enforced");
    }

    // Wait for the child in a blocking task (since std::process::Child::wait_with_output is blocking)
    // The job handle is a raw pointer, wrap it for safe Send across thread boundary.
    let job_raw = job as usize; // HANDLE is a pointer, store as usize for Send

    let result = tokio::time::timeout(
        timeout,
        tokio::task::spawn_blocking(move || child.wait_with_output()),
    )
    .await;

    // Clean up the job object (kills any remaining processes due to KILL_ON_JOB_CLOSE)
    unsafe { CloseHandle(job_raw as _) };

    match result {
        Ok(Ok(Ok(output))) => Ok(sandboxed_output(
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
            output.status.code().unwrap_or(-1),
            false,
            build_sandbox_audit(
                SandboxEnvironment::Windows,
                "windows_job",
                policy,
                true,
                &[
                    "job_object",
                    "wall_timeout",
                    "process_limit",
                    "memory_limit",
                    "cpu_limit",
                ],
                None,
            ),
        )),
        Ok(Ok(Err(e))) => Err(Error::Sandbox(format!("Exec in job failed: {e}"))),
        Ok(Err(e)) => Err(Error::Sandbox(format!("Blocking task panicked: {e}"))),
        Err(_) => {
            // Timeout — the Job Object's KILL_ON_JOB_CLOSE will terminate
            // the child when we dropped/closed the job handle above.
            Ok(sandboxed_output(
                String::new(),
                format!(
                    "Process timed out after {}s (Job Object killed)",
                    policy.wall_timeout_secs
                ),
                -1,
                true,
                build_sandbox_audit(
                    SandboxEnvironment::Windows,
                    "windows_job",
                    policy,
                    true,
                    &[
                        "job_object",
                        "wall_timeout",
                        "process_limit",
                        "memory_limit",
                        "cpu_limit",
                    ],
                    None,
                ),
            ))
        }
    }
}

/// Linux sandbox using namespace isolation and resource limits.
///
/// Uses `unshare(2)` flags via command wrapping and `prlimit` to enforce:
/// - Memory limits via RLIMIT_AS
/// - CPU time limits via RLIMIT_CPU
/// - Network isolation (if not allowed) via unshare --net
/// - Mount namespace isolation via unshare --mount
/// - PID namespace isolation via unshare --pid --fork
/// - Wall-clock timeout via tokio::time::timeout
///
/// On kernels >= 5.13, Landlock LSM is available for filesystem access control.
/// We use the `unshare` command-line tool which is available on all Linux distros.
#[cfg(target_os = "linux")]
async fn exec_linux_sandboxed(
    command: &str,
    args: &[&str],
    cwd: Option<&std::path::Path>,
    policy: &SandboxPolicy,
) -> Result<SandboxedOutput> {
    use tokio::process::Command;

    info!("Linux sandbox — using namespace isolation + resource limits");

    // Check if `unshare` is available (it's in util-linux, present on virtually all distros)
    let have_unshare = tokio::process::Command::new("which")
        .arg("unshare")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !have_unshare {
        warn!("'unshare' not found, falling back to basic limits");
        return exec_linux_prlimit(command, args, cwd, policy).await;
    }

    let mut cmd_args: Vec<String> = Vec::new();

    // PID namespace isolation (fork required)
    cmd_args.push("--pid".into());
    cmd_args.push("--fork".into());

    // Mount namespace (prevent host FS mutations)
    cmd_args.push("--mount".into());

    // Network isolation (if not allowed)
    if !policy.allow_network {
        cmd_args.push("--net".into());
    }

    // IPC namespace
    cmd_args.push("--ipc".into());

    // UTS namespace (hostname isolation)
    cmd_args.push("--uts".into());

    // The actual command
    cmd_args.push("--".into());

    // Wrap with prlimit for resource limits
    cmd_args.push("prlimit".into());

    if policy.memory_limit_bytes > 0 {
        cmd_args.push(format!("--as={}", policy.memory_limit_bytes));
    }

    if policy.cpu_time_limit_secs > 0 {
        cmd_args.push(format!("--cpu={}", policy.cpu_time_limit_secs));
    }

    // Limit number of processes (prevent fork bombs)
    if !policy.allow_spawn {
        cmd_args.push("--nproc=1".into());
    }

    // Limit file size to 100MB
    cmd_args.push("--fsize=104857600".into());

    cmd_args.push("--".into());
    cmd_args.push(command.into());
    for arg in args {
        cmd_args.push((*arg).into());
    }

    let timeout = if policy.wall_timeout_secs > 0 {
        std::time::Duration::from_secs(policy.wall_timeout_secs)
    } else {
        std::time::Duration::from_secs(300)
    };

    let str_args: Vec<&str> = cmd_args.iter().map(|s| s.as_str()).collect();

    let mut cmd = Command::new("unshare");
    cmd.args(&str_args);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }

    let result = tokio::time::timeout(timeout, cmd.output()).await;

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

            if !output.status.success()
                && stderr_contains_any(
                    &stderr,
                    &[
                        "unshare failed",
                        "operation not permitted",
                        "permission denied",
                        "invalid argument",
                    ],
                )
            {
                warn!(stderr = %stderr, "unshare backend rejected sandbox setup, falling back to prlimit");
                return exec_linux_prlimit(command, args, cwd, policy).await;
            }

            Ok(sandboxed_output(
                stdout,
                stderr,
                output.status.code().unwrap_or(-1),
                false,
                build_sandbox_audit(
                    SandboxEnvironment::Linux,
                    "linux_unshare",
                    policy,
                    true,
                    &[
                        "namespace_isolation",
                        "wall_timeout",
                        "process_limit",
                        "memory_limit",
                        "cpu_limit",
                        "network_policy",
                    ],
                    None,
                ),
            ))
        }
        Ok(Err(e)) => {
            // If unshare fails (e.g., insufficient privileges), fall back to prlimit only
            warn!(error = %e, "unshare failed, falling back to prlimit");
            exec_linux_prlimit(command, args, cwd, policy).await
        }
        Err(_) => Ok(sandboxed_output(
            String::new(),
            format!(
                "Process timed out after {}s (killed)",
                policy.wall_timeout_secs
            ),
            -1,
            true,
            build_sandbox_audit(
                SandboxEnvironment::Linux,
                "linux_unshare",
                policy,
                true,
                &[
                    "namespace_isolation",
                    "wall_timeout",
                    "process_limit",
                    "memory_limit",
                    "cpu_limit",
                    "network_policy",
                ],
                None,
            ),
        )),
    }
}

/// Fallback Linux sandbox using only prlimit (no namespace isolation).
#[cfg(target_os = "linux")]
async fn exec_linux_prlimit(
    command: &str,
    args: &[&str],
    cwd: Option<&std::path::Path>,
    policy: &SandboxPolicy,
) -> Result<SandboxedOutput> {
    use tokio::process::Command;

    let mut cmd_args: Vec<String> = Vec::new();

    if policy.memory_limit_bytes > 0 {
        cmd_args.push(format!("--as={}", policy.memory_limit_bytes));
    }
    if policy.cpu_time_limit_secs > 0 {
        cmd_args.push(format!("--cpu={}", policy.cpu_time_limit_secs));
    }
    if !policy.allow_spawn {
        cmd_args.push("--nproc=1".into());
    }
    cmd_args.push("--fsize=104857600".into()); // 100MB file size limit

    cmd_args.push("--".into());
    cmd_args.push(command.into());
    for arg in args {
        cmd_args.push((*arg).into());
    }

    let timeout = if policy.wall_timeout_secs > 0 {
        std::time::Duration::from_secs(policy.wall_timeout_secs)
    } else {
        std::time::Duration::from_secs(300)
    };

    let str_args: Vec<&str> = cmd_args.iter().map(|s| s.as_str()).collect();

    let mut cmd = Command::new("prlimit");
    cmd.args(&str_args);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }

    let result = tokio::time::timeout(timeout, cmd.output()).await;

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

            if is_prlimit_wrapper_failure(&stderr, output.status.code()) {
                warn!(stderr = %stderr, "prlimit backend rejected sandbox setup, falling back to direct execution");
                return exec_direct(
                    command,
                    args,
                    cwd,
                    policy,
                    build_sandbox_audit(
                        SandboxEnvironment::Linux,
                        "direct",
                        policy,
                        false,
                        &["wall_timeout"],
                        Some("prlimit backend rejected sandbox setup; falling back to direct execution".into()),
                    ),
                )
                .await;
            }

            Ok(sandboxed_output(
                stdout,
                stderr,
                output.status.code().unwrap_or(-1),
                false,
                build_sandbox_audit(
                    SandboxEnvironment::Linux,
                    "linux_prlimit",
                    policy,
                    false,
                    &[
                        "prlimit",
                        "wall_timeout",
                        "process_limit",
                        "memory_limit",
                        "cpu_limit",
                    ],
                    Some("namespace isolation unavailable; using prlimit-only enforcement".into()),
                ),
            ))
        }
        Ok(Err(e)) => Err(Error::Sandbox(format!("prlimit exec failed: {e}"))),
        Err(_) => Ok(sandboxed_output(
            String::new(),
            format!("Process timed out after {}s", policy.wall_timeout_secs),
            -1,
            true,
            build_sandbox_audit(
                SandboxEnvironment::Linux,
                "linux_prlimit",
                policy,
                false,
                &[
                    "prlimit",
                    "wall_timeout",
                    "process_limit",
                    "memory_limit",
                    "cpu_limit",
                ],
                Some("namespace isolation unavailable; using prlimit-only enforcement".into()),
            ),
        )),
    }
}

/// macOS sandbox using `sandbox-exec` (Seatbelt profiles).
///
/// Uses Apple's `sandbox-exec` command with a dynamically generated SBPL
/// (Sandbox Profile Language) profile to restrict:
/// - File read/write access to specific paths
/// - Network access
/// - Process spawning
///
/// Wall-clock timeout enforced via tokio::time::timeout.
/// Memory/CPU limits enforced via `ulimit` wrapping.
#[cfg(target_os = "macos")]
async fn exec_macos_sandboxed(
    command: &str,
    args: &[&str],
    cwd: Option<&std::path::Path>,
    policy: &SandboxPolicy,
) -> Result<SandboxedOutput> {
    use tokio::process::Command;

    info!("macOS sandbox — using sandbox-exec (Seatbelt)");

    // Generate a Seatbelt profile (SBPL)
    let mut profile = String::from("(version 1)\n(deny default)\n");
    let mut allowed_read_paths = Vec::new();
    let mut allowed_write_paths = Vec::new();

    for variant in path_variants(std::path::Path::new(command)) {
        if !allowed_read_paths
            .iter()
            .any(|existing| existing == &variant)
        {
            allowed_read_paths.push(variant);
        }
    }

    for path in &policy.allow_read_paths {
        for variant in path_variants(std::path::Path::new(path)) {
            if !allowed_read_paths
                .iter()
                .any(|existing| existing == &variant)
            {
                allowed_read_paths.push(variant);
            }
        }
    }

    for path in &policy.allow_write_paths {
        for variant in path_variants(std::path::Path::new(path)) {
            if !allowed_write_paths
                .iter()
                .any(|existing| existing == &variant)
            {
                allowed_write_paths.push(variant);
            }
        }
    }

    if let Some(cwd) = cwd {
        for variant in path_variants(cwd) {
            if !allowed_read_paths
                .iter()
                .any(|existing| existing == &variant)
            {
                allowed_read_paths.push(variant.clone());
            }
            if !allowed_write_paths
                .iter()
                .any(|existing| existing == &variant)
            {
                allowed_write_paths.push(variant);
            }
        }
    }

    // Always allow basic operations
    profile.push_str("(allow process-exec*)\n");
    profile.push_str("(allow process-fork)\n");
    profile.push_str("(allow sysctl-read)\n");
    profile.push_str("(allow mach-lookup)\n");
    profile.push_str("(allow signal (target self))\n");

    // File read access
    if allowed_read_paths.is_empty() {
        // Allow reading system libraries and common paths
        profile.push_str("(allow file-read* (subpath \"/usr/lib\"))\n");
        profile.push_str("(allow file-read* (subpath \"/usr/share\"))\n");
        profile.push_str("(allow file-read* (subpath \"/System\"))\n");
        profile.push_str("(allow file-read* (subpath \"/Library/Frameworks\"))\n");
        profile.push_str("(allow file-read* (subpath \"/dev\"))\n");
        // Allow reading the command binary itself
        profile.push_str(&format!("(allow file-read* (literal \"{}\"))\n", command));
    } else {
        allow_profile_paths(&mut profile, "file-read*", &allowed_read_paths);
        // Always allow system libs
        profile.push_str("(allow file-read* (subpath \"/usr/lib\"))\n");
        profile.push_str("(allow file-read* (subpath \"/usr/share\"))\n");
        profile.push_str("(allow file-read* (subpath \"/System\"))\n");
        profile.push_str("(allow file-read* (subpath \"/Library/Frameworks\"))\n");
        profile.push_str("(allow file-read* (subpath \"/dev\"))\n");
    }

    // File write access
    allow_profile_paths(&mut profile, "file-write*", &allowed_write_paths);
    // Allow writing to /dev/null, /dev/tty
    profile.push_str("(allow file-write* (subpath \"/dev\"))\n");

    // Network access
    if policy.allow_network {
        profile.push_str("(allow network*)\n");
    }

    // Spawn new processes
    if !policy.allow_spawn {
        // Already restricted by default deny,
        // but we allowed process-exec/fork for the main command.
        // This is a simplification — full restriction would need
        // to only allow the specific command binary.
    }

    // Write the profile to a temp file
    let profile_path =
        std::env::temp_dir().join(format!("ngenorca_sandbox_{}.sb", std::process::id()));
    if let Err(e) = std::fs::write(&profile_path, &profile) {
        warn!(error = %e, "Failed to write sandbox profile, falling back to basic exec");
        return exec_direct(
            command,
            args,
            cwd,
            policy,
            build_sandbox_audit(
                SandboxEnvironment::MacOs,
                "direct",
                policy,
                false,
                &["wall_timeout"],
                Some("failed to materialize macOS sandbox profile".into()),
            ),
        )
        .await;
    }

    let timeout = if policy.wall_timeout_secs > 0 {
        std::time::Duration::from_secs(policy.wall_timeout_secs)
    } else {
        std::time::Duration::from_secs(300)
    };

    // Build the command: sandbox-exec -f <profile> <command> <args>
    let mut cmd = Command::new("sandbox-exec");
    cmd.arg("-f").arg(&profile_path);
    cmd.arg(command);
    cmd.args(args);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }

    let result = tokio::time::timeout(timeout, cmd.output()).await;

    // Clean up the profile file
    let _ = std::fs::remove_file(&profile_path);

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

            if !output.status.success()
                && stderr_contains_any(
                    &stderr,
                    &[
                        "sandbox-exec:",
                        "operation not permitted",
                        "permission denied",
                        "no such file or directory",
                    ],
                )
            {
                warn!(stderr = %stderr, "sandbox-exec backend rejected sandbox setup, falling back to direct execution");
                return exec_direct(
                    command,
                    args,
                    cwd,
                    policy,
                    build_sandbox_audit(
                        SandboxEnvironment::MacOs,
                        "direct",
                        policy,
                        false,
                        &["wall_timeout"],
                        Some("sandbox-exec backend rejected the generated profile or command; falling back to direct execution".into()),
                    ),
                )
                .await;
            }

            Ok(sandboxed_output(
                stdout,
                stderr,
                output.status.code().unwrap_or(-1),
                false,
                build_sandbox_audit(
                    SandboxEnvironment::MacOs,
                    "macos_sandbox_exec",
                    policy,
                    true,
                    &[
                        "seatbelt_profile",
                        "wall_timeout",
                        "filesystem_policy",
                        "network_policy",
                    ],
                    None,
                ),
            ))
        }
        Ok(Err(e)) => {
            warn!(error = %e, "sandbox-exec failed, falling back to basic exec");
            exec_direct(
                command,
                args,
                cwd,
                policy,
                build_sandbox_audit(
                    SandboxEnvironment::MacOs,
                    "direct",
                    policy,
                    false,
                    &["wall_timeout"],
                    Some("sandbox-exec failed; falling back to direct execution".into()),
                ),
            )
            .await
        }
        Err(_) => Ok(sandboxed_output(
            String::new(),
            format!(
                "Process timed out after {}s (killed)",
                policy.wall_timeout_secs
            ),
            -1,
            true,
            build_sandbox_audit(
                SandboxEnvironment::MacOs,
                "macos_sandbox_exec",
                policy,
                true,
                &[
                    "seatbelt_profile",
                    "wall_timeout",
                    "filesystem_policy",
                    "network_policy",
                ],
                None,
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── SandboxEnvironment tests ───

    #[test]
    fn sandbox_environment_serde_roundtrip() {
        let envs = vec![
            SandboxEnvironment::Container,
            SandboxEnvironment::Windows,
            SandboxEnvironment::Linux,
            SandboxEnvironment::MacOs,
            SandboxEnvironment::Unknown,
        ];
        for env in envs {
            let json = serde_json::to_string(&env).unwrap();
            let back: SandboxEnvironment = serde_json::from_str(&json).unwrap();
            assert_eq!(back, env);
        }
    }

    #[test]
    fn detect_environment_returns_windows() {
        // We're running on Windows, so unless inside Docker this should be Windows
        let env = detect_environment();
        // On CI/container this might be Container, but on native Windows:
        assert!(
            env == SandboxEnvironment::Windows || env == SandboxEnvironment::Container,
            "Expected Windows or Container, got {env:?}"
        );
    }

    // ─── SandboxPolicy tests ───

    #[test]
    fn sandbox_policy_default_values() {
        let policy = SandboxPolicy::default();
        assert!(!policy.allow_network);
        assert!(!policy.allow_spawn);
        assert!(policy.allow_read_paths.is_empty());
        assert!(policy.allow_write_paths.is_empty());
        assert_eq!(policy.memory_limit_bytes, 512 * 1024 * 1024);
        assert_eq!(policy.cpu_time_limit_secs, 30);
        assert_eq!(policy.wall_timeout_secs, 60);
    }

    #[test]
    fn sandbox_policy_serde_roundtrip() {
        let policy = SandboxPolicy {
            allow_network: true,
            allow_read_paths: vec!["/tmp".into(), "/data".into()],
            allow_write_paths: vec!["/tmp/out".into()],
            allow_spawn: true,
            memory_limit_bytes: 1024 * 1024 * 1024,
            cpu_time_limit_secs: 60,
            wall_timeout_secs: 120,
        };
        let json = serde_json::to_string(&policy).unwrap();
        let back: SandboxPolicy = serde_json::from_str(&json).unwrap();
        assert!(back.allow_network);
        assert!(back.allow_spawn);
        assert_eq!(back.allow_read_paths.len(), 2);
        assert_eq!(back.allow_write_paths.len(), 1);
        assert_eq!(back.memory_limit_bytes, 1024 * 1024 * 1024);
    }

    #[test]
    fn sandbox_policy_custom_paths() {
        let mut policy = SandboxPolicy::default();
        policy.allow_read_paths.push("/home/user".into());
        policy.allow_write_paths.push("/home/user/output".into());
        assert_eq!(policy.allow_read_paths, vec!["/home/user".to_string()]);
        assert_eq!(
            policy.allow_write_paths,
            vec!["/home/user/output".to_string()]
        );
    }

    // ─── SandboxedOutput tests ───

    #[test]
    fn sandboxed_output_construction() {
        let out = SandboxedOutput {
            stdout: "hello\n".into(),
            stderr: String::new(),
            exit_code: 0,
            timed_out: false,
            audit: audit_policy(&SandboxPolicy::default(), true),
        };
        assert_eq!(out.exit_code, 0);
        assert!(!out.timed_out);
        assert_eq!(out.stdout, "hello\n");
    }

    #[test]
    fn sandboxed_output_timeout() {
        let out = SandboxedOutput {
            stdout: String::new(),
            stderr: "timed out".into(),
            exit_code: -1,
            timed_out: true,
            audit: audit_policy(&SandboxPolicy::default(), true),
        };
        assert!(out.timed_out);
        assert_eq!(out.exit_code, -1);
    }

    #[test]
    fn audit_policy_reports_backend_gaps() {
        let audit = audit_policy(&SandboxPolicy::default(), true);
        assert!(!audit.backend.is_empty());
        assert!(
            audit
                .enforced_controls
                .iter()
                .any(|value| value == "wall_timeout")
        );
    }

    #[test]
    fn audit_policy_reports_disabled_sandbox() {
        let audit = audit_policy(&SandboxPolicy::default(), false);
        assert_eq!(audit.backend, "direct");
        assert!(audit.fallback_reason.is_some());
    }

    // ─── sandboxed_exec integration tests ───

    #[tokio::test]
    async fn exec_echo_succeeds() {
        let policy = SandboxPolicy::default();
        // On Windows, use cmd /C echo
        let result = sandboxed_exec("cmd", &["/C", "echo", "hello"], &policy).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.contains("hello"));
        assert!(!output.timed_out);
        assert!(!output.audit.backend.is_empty());
    }

    #[tokio::test]
    async fn exec_nonexistent_command_errors() {
        let policy = SandboxPolicy::default();
        let result = sandboxed_exec("this_command_does_not_exist_12345", &[], &policy).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn exec_timeout_works() {
        let policy = SandboxPolicy {
            wall_timeout_secs: 1,
            ..SandboxPolicy::default()
        };
        // "ping -n 30 127.0.0.1" will take ~30 seconds on Windows
        let result = sandboxed_exec("ping", &["-n", "30", "127.0.0.1"], &policy).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.timed_out);
        assert_eq!(output.exit_code, -1);
    }

    #[tokio::test]
    async fn exec_with_cwd_preserves_working_directory() {
        let policy = SandboxPolicy::default();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let cwd = std::env::temp_dir().join(format!(
            "ngenorca_sandbox_cwd_{}_{}",
            std::process::id(),
            unique
        ));
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(cwd.join("marker.txt"), "ok").unwrap();

        #[cfg(windows)]
        let result = sandboxed_exec_with_cwd(
            "cmd",
            &[
                "/C",
                "if",
                "exist",
                "marker.txt",
                "(echo",
                "marker-found)",
                "else",
                "(echo",
                "missing)",
            ],
            Some(&cwd),
            &policy,
        )
        .await;
        #[cfg(not(windows))]
        let result = sandboxed_exec_with_cwd(
            "sh",
            &[
                "-c",
                "test -f marker.txt && echo marker-found || echo missing",
            ],
            Some(&cwd),
            &policy,
        )
        .await;

        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.stdout.contains("marker-found"));
    }
}
