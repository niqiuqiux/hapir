use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// CLI settings stored in settings.json.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_id_confirmed_by_server: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_auto_start_when_running_happy: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli_api_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_url: Option<String>,
    /// Legacy field name (for migration)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_url: Option<String>,
}

/// Runner state persisted locally.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunnerLocalState {
    pub pid: u32,
    pub http_port: u16,
    pub start_time: String,
    pub started_with_cli_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_with_cli_mtime_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_heartbeat: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_log_path: Option<String>,
}

pub fn read_settings(path: &Path) -> Result<Settings> {
    if !path.exists() {
        return Ok(Settings::default());
    }
    let content = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content).unwrap_or_default())
}

pub fn write_settings(path: &Path, settings: &Settings) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(settings)?;
    std::fs::write(path, content)?;
    Ok(())
}

/// Atomically update settings with file locking for multi-process safety.
pub fn update_settings(
    path: &Path,
    updater: impl FnOnce(&mut Settings),
) -> Result<Settings> {
    const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(100);
    const MAX_LOCK_ATTEMPTS: u32 = 50;
    const STALE_LOCK_TIMEOUT: Duration = Duration::from_secs(10);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let lock_path = path.with_extension("json.lock");
    let tmp_path = path.with_extension("json.tmp");

    // Acquire exclusive lock with retries
    let mut attempts = 0;
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(_file) => break,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                attempts += 1;
                if attempts >= MAX_LOCK_ATTEMPTS {
                    bail!("failed to acquire settings lock after 5 seconds");
                }

                // Check for stale lock
                if let Ok(meta) = std::fs::metadata(&lock_path) {
                    if let Ok(modified) = meta.modified() {
                        if SystemTime::now()
                            .duration_since(modified)
                            .unwrap_or(Duration::ZERO)
                            > STALE_LOCK_TIMEOUT
                        {
                            let _ = std::fs::remove_file(&lock_path);
                            continue;
                        }
                    }
                }

                std::thread::sleep(LOCK_RETRY_INTERVAL);
            }
            Err(e) => return Err(e.into()),
        }
    }

    let result = (|| -> Result<Settings> {
        let mut settings = read_settings(path)?;
        updater(&mut settings);

        // Write atomically via rename
        let content = serde_json::to_string_pretty(&settings)?;
        std::fs::write(&tmp_path, &content)?;
        std::fs::rename(&tmp_path, path)?;

        Ok(settings)
    })();

    // Always release lock
    let _ = std::fs::remove_file(&lock_path);

    result
}

pub fn read_runner_state(path: &Path) -> Option<RunnerLocalState> {
    if !path.exists() {
        return None;
    }
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn write_runner_state(path: &Path, state: &RunnerLocalState) -> Result<()> {
    let content = serde_json::to_string_pretty(state)?;
    std::fs::write(path, content)?;
    Ok(())
}

pub fn clear_runner_state(state_path: &Path, lock_path: &Path) {
    let _ = std::fs::remove_file(state_path);
    let _ = std::fs::remove_file(lock_path);
}

/// Acquire an exclusive runner lock file. Returns a guard that releases on drop.
pub fn acquire_runner_lock(lock_path: &Path, max_attempts: u32) -> Option<RunnerLockGuard> {
    for attempt in 1..=max_attempts {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)
        {
            Ok(mut file) => {
                use std::io::Write;
                let _ = write!(file, "{}", std::process::id());
                return Some(RunnerLockGuard {
                    path: lock_path.to_path_buf(),
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Check if holder process is still alive
                if let Ok(content) = std::fs::read_to_string(lock_path) {
                    if let Ok(pid) = content.trim().parse::<u32>() {
                        if !is_process_alive(pid) {
                            let _ = std::fs::remove_file(lock_path);
                            continue;
                        }
                    }
                }

                if attempt == max_attempts {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(200 * attempt as u64));
            }
            Err(_) => {
                if attempt == max_attempts {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(200 * attempt as u64));
            }
        }
    }
    None
}

pub struct RunnerLockGuard {
    path: std::path::PathBuf,
}

impl Drop for RunnerLockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // kill(pid, 0) checks if process exists without sending a signal
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn settings_roundtrip() {
        let dir = std::env::temp_dir().join("hapi_test_settings");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.json");
        let _ = std::fs::remove_file(&path);

        let settings = Settings {
            machine_id: Some("test-machine".into()),
            cli_api_token: Some("test-token".into()),
            ..Default::default()
        };

        write_settings(&path, &settings).unwrap();
        let loaded = read_settings(&path).unwrap();
        assert_eq!(loaded.machine_id.as_deref(), Some("test-machine"));
        assert_eq!(loaded.cli_api_token.as_deref(), Some("test-token"));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn update_settings_atomic() {
        let dir = std::env::temp_dir().join("hapi_test_update");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.json");
        let _ = std::fs::remove_file(&path);

        // Write initial
        write_settings(&path, &Settings::default()).unwrap();

        // Update atomically
        let result = update_settings(&path, |s| {
            s.machine_id = Some("updated".into());
        })
        .unwrap();

        assert_eq!(result.machine_id.as_deref(), Some("updated"));

        // Re-read to verify
        let loaded = read_settings(&path).unwrap();
        assert_eq!(loaded.machine_id.as_deref(), Some("updated"));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn runner_state_roundtrip() {
        let dir = std::env::temp_dir().join("hapi_test_runner");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("runner.state.json");
        let _ = std::fs::remove_file(&path);

        let state = RunnerLocalState {
            pid: 12345,
            http_port: 8080,
            start_time: "2025-01-01T00:00:00Z".into(),
            started_with_cli_version: "0.1.0".into(),
            started_with_cli_mtime_ms: None,
            last_heartbeat: None,
            runner_log_path: None,
        };

        write_runner_state(&path, &state).unwrap();
        let loaded = read_runner_state(&path).unwrap();
        assert_eq!(loaded.pid, 12345);
        assert_eq!(loaded.http_port, 8080);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn read_missing_settings_returns_default() {
        let path = PathBuf::from("/nonexistent/path/settings.json");
        let settings = read_settings(&path).unwrap();
        assert!(settings.machine_id.is_none());
    }
}
