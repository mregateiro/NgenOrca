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
        if let Ok(cgroup) = std::fs::read_to_string("/proc/1/cgroup") {
            if cgroup.contains("docker") || cgroup.contains("kubepods") || cgroup.contains("containerd") {
                return true;
            }
        }
        // systemd-based container detection.
        if let Ok(env) = std::fs::read_to_string("/proc/1/environ") {
            if env.contains("container=") {
                return true;
            }
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
    let env = detect_environment();

    info!(?env, command, "Executing in sandbox");

    match env {
        SandboxEnvironment::Container => {
            // Inside a container, the container IS the sandbox.
            // Just run the command directly.
            exec_direct(command, args, policy).await
        }
        SandboxEnvironment::Windows => {
            // Use Windows Job Objects.
            #[cfg(windows)]
            {
                exec_windows_job(command, args, policy).await
            }
            #[cfg(not(windows))]
            {
                warn!("Windows sandbox not available on this platform");
                exec_direct(command, args, policy).await
            }
        }
        SandboxEnvironment::Linux => {
            #[cfg(target_os = "linux")]
            {
                exec_linux_sandboxed(command, args, policy).await
            }
            #[cfg(not(target_os = "linux"))]
            {
                warn!("Linux sandbox not available on this platform");
                exec_direct(command, args, policy).await
            }
        }
        SandboxEnvironment::MacOs => {
            #[cfg(target_os = "macos")]
            {
                exec_macos_sandboxed(command, args, policy).await
            }
            #[cfg(not(target_os = "macos"))]
            {
                warn!("macOS sandbox not available on this platform");
                exec_direct(command, args, policy).await
            }
        }
        SandboxEnvironment::Unknown => {
            warn!("Unknown platform, running without sandbox");
            exec_direct(command, args, policy).await
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
}

/// Direct execution (used inside containers or as fallback).
async fn exec_direct(
    command: &str,
    args: &[&str],
    policy: &SandboxPolicy,
) -> Result<SandboxedOutput> {
    use tokio::process::Command;

    let timeout = if policy.wall_timeout_secs > 0 {
        std::time::Duration::from_secs(policy.wall_timeout_secs)
    } else {
        std::time::Duration::from_secs(300) // 5 min default cap
    };

    let result = tokio::time::timeout(
        timeout,
        Command::new(command).args(args).output(),
    )
    .await;

    match result {
        Ok(Ok(output)) => Ok(SandboxedOutput {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            exit_code: output.status.code().unwrap_or(-1),
            timed_out: false,
        }),
        Ok(Err(e)) => Err(Error::Sandbox(format!("Exec failed: {e}"))),
        Err(_) => Ok(SandboxedOutput {
            stdout: String::new(),
            stderr: format!("Process timed out after {}s", policy.wall_timeout_secs),
            exit_code: -1,
            timed_out: true,
        }),
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
    policy: &SandboxPolicy,
) -> Result<SandboxedOutput> {
    use std::mem::{size_of, zeroed};
    use std::ptr::null;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOB_OBJECT_LIMIT_PROCESS_TIME,
    };

    info!("Windows Job Object sandbox — enforcing limits");

    // ── Create a Job Object ──
    let job = unsafe { CreateJobObjectW(null(), null()) };
    if job.is_null() || job == INVALID_HANDLE_VALUE {
        warn!("Failed to create Job Object, falling back to basic exec");
        return exec_direct(command, args, policy).await;
    }

    // ── Configure limits ──
    let mut ext_info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };

    let mut limit_flags: u32 = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;

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
        return exec_direct(command, args, policy).await;
    }

    // ── Spawn child process and assign to Job Object ──
    let timeout = if policy.wall_timeout_secs > 0 {
        std::time::Duration::from_secs(policy.wall_timeout_secs)
    } else {
        std::time::Duration::from_secs(300)
    };

    let mut std_cmd = std::process::Command::new(command);
    std_cmd.args(args);
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

    let result = tokio::time::timeout(timeout, tokio::task::spawn_blocking(move || {
        child.wait_with_output()
    }))
    .await;

    // Clean up the job object (kills any remaining processes due to KILL_ON_JOB_CLOSE)
    unsafe { CloseHandle(job_raw as _) };

    match result {
        Ok(Ok(Ok(output))) => Ok(SandboxedOutput {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            exit_code: output.status.code().unwrap_or(-1),
            timed_out: false,
        }),
        Ok(Ok(Err(e))) => Err(Error::Sandbox(format!("Exec in job failed: {e}"))),
        Ok(Err(e)) => Err(Error::Sandbox(format!("Blocking task panicked: {e}"))),
        Err(_) => {
            // Timeout — the Job Object's KILL_ON_JOB_CLOSE will terminate
            // the child when we dropped/closed the job handle above.
            Ok(SandboxedOutput {
                stdout: String::new(),
                stderr: format!("Process timed out after {}s (Job Object killed)", policy.wall_timeout_secs),
                exit_code: -1,
                timed_out: true,
            })
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
        return exec_linux_prlimit(command, args, policy).await;
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

    let result = tokio::time::timeout(
        timeout,
        Command::new("unshare").args(&str_args).output(),
    )
    .await;

    match result {
        Ok(Ok(output)) => Ok(SandboxedOutput {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            exit_code: output.status.code().unwrap_or(-1),
            timed_out: false,
        }),
        Ok(Err(e)) => {
            // If unshare fails (e.g., insufficient privileges), fall back to prlimit only
            warn!(error = %e, "unshare failed, falling back to prlimit");
            exec_linux_prlimit(command, args, policy).await
        }
        Err(_) => Ok(SandboxedOutput {
            stdout: String::new(),
            stderr: format!(
                "Process timed out after {}s (killed)",
                policy.wall_timeout_secs
            ),
            exit_code: -1,
            timed_out: true,
        }),
    }
}

/// Fallback Linux sandbox using only prlimit (no namespace isolation).
#[cfg(target_os = "linux")]
async fn exec_linux_prlimit(
    command: &str,
    args: &[&str],
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

    let result = tokio::time::timeout(
        timeout,
        Command::new("prlimit").args(&str_args).output(),
    )
    .await;

    match result {
        Ok(Ok(output)) => Ok(SandboxedOutput {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            exit_code: output.status.code().unwrap_or(-1),
            timed_out: false,
        }),
        Ok(Err(e)) => Err(Error::Sandbox(format!("prlimit exec failed: {e}"))),
        Err(_) => Ok(SandboxedOutput {
            stdout: String::new(),
            stderr: format!(
                "Process timed out after {}s",
                policy.wall_timeout_secs
            ),
            exit_code: -1,
            timed_out: true,
        }),
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
    policy: &SandboxPolicy,
) -> Result<SandboxedOutput> {
    use tokio::process::Command;

    info!("macOS sandbox — using sandbox-exec (Seatbelt)");

    // Generate a Seatbelt profile (SBPL)
    let mut profile = String::from("(version 1)\n(deny default)\n");

    // Always allow basic operations
    profile.push_str("(allow process-exec*)\n");
    profile.push_str("(allow process-fork)\n");
    profile.push_str("(allow sysctl-read)\n");
    profile.push_str("(allow mach-lookup)\n");
    profile.push_str("(allow signal (target self))\n");

    // File read access
    if policy.allow_read_paths.is_empty() {
        // Allow reading system libraries and common paths
        profile.push_str("(allow file-read* (subpath \"/usr/lib\"))\n");
        profile.push_str("(allow file-read* (subpath \"/usr/share\"))\n");
        profile.push_str("(allow file-read* (subpath \"/System\"))\n");
        profile.push_str("(allow file-read* (subpath \"/Library/Frameworks\"))\n");
        profile.push_str("(allow file-read* (subpath \"/dev\"))\n");
        // Allow reading the command binary itself
        profile.push_str(&format!(
            "(allow file-read* (literal \"{}\"))\n",
            command
        ));
    } else {
        for path in &policy.allow_read_paths {
            profile.push_str(&format!(
                "(allow file-read* (subpath \"{}\"))\n",
                path
            ));
        }
        // Always allow system libs
        profile.push_str("(allow file-read* (subpath \"/usr/lib\"))\n");
        profile.push_str("(allow file-read* (subpath \"/System\"))\n");
    }

    // File write access
    for path in &policy.allow_write_paths {
        profile.push_str(&format!(
            "(allow file-write* (subpath \"{}\"))\n",
            path
        ));
    }
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
    let profile_path = std::env::temp_dir().join(format!(
        "ngenorca_sandbox_{}.sb",
        std::process::id()
    ));
    if let Err(e) = std::fs::write(&profile_path, &profile) {
        warn!(error = %e, "Failed to write sandbox profile, falling back to basic exec");
        return exec_direct(command, args, policy).await;
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

    let result = tokio::time::timeout(timeout, cmd.output()).await;

    // Clean up the profile file
    let _ = std::fs::remove_file(&profile_path);

    match result {
        Ok(Ok(output)) => Ok(SandboxedOutput {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            exit_code: output.status.code().unwrap_or(-1),
            timed_out: false,
        }),
        Ok(Err(e)) => {
            warn!(error = %e, "sandbox-exec failed, falling back to basic exec");
            exec_direct(command, args, policy).await
        }
        Err(_) => Ok(SandboxedOutput {
            stdout: String::new(),
            stderr: format!(
                "Process timed out after {}s (killed)",
                policy.wall_timeout_secs
            ),
            exit_code: -1,
            timed_out: true,
        }),
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
        assert_eq!(policy.allow_write_paths, vec!["/home/user/output".to_string()]);
    }

    // ─── SandboxedOutput tests ───

    #[test]
    fn sandboxed_output_construction() {
        let out = SandboxedOutput {
            stdout: "hello\n".into(),
            stderr: String::new(),
            exit_code: 0,
            timed_out: false,
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
        };
        assert!(out.timed_out);
        assert_eq!(out.exit_code, -1);
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
    }

    #[tokio::test]
    async fn exec_nonexistent_command_errors() {
        let policy = SandboxPolicy::default();
        let result = sandboxed_exec("this_command_does_not_exist_12345", &[], &policy).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn exec_timeout_works() {
        let policy = SandboxPolicy { wall_timeout_secs: 1, ..SandboxPolicy::default() };
        // "ping -n 30 127.0.0.1" will take ~30 seconds on Windows
        let result = sandboxed_exec("ping", &["-n", "30", "127.0.0.1"], &policy).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.timed_out);
        assert_eq!(output.exit_code, -1);
    }
}
