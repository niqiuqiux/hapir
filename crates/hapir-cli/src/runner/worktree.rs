use std::path::Path;

use rand::Rng;
use tracing::debug;

/// Metadata about a created worktree.
#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    pub base_path: String,
    pub worktree_path: String,
    pub branch: String,
    pub name: String,
    pub created_at: i64,
}

const MAX_ATTEMPTS: usize = 5;

/// Run a git command in `cwd` and return stdout.
async fn run_git(args: &[&str], cwd: &Path) -> Result<String, String> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .await
        .map_err(|e| format!("Failed to run git: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let msg = stderr.trim();
        let msg = if msg.is_empty() { stdout.trim() } else { msg };
        return Err(msg.to_string());
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Resolve the git repository root from a path.
async fn resolve_repo_root(base_path: &Path) -> Result<String, String> {
    let root = run_git(&["rev-parse", "--show-toplevel"], base_path).await?;
    if root.is_empty() {
        return Err("Unable to resolve Git repository root.".to_string());
    }
    Ok(root)
}

/// Slugify a string for use in branch/directory names.
/// Matches TS: `.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '')`
fn to_slug(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut prev_was_dash = false;
    for c in value.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            result.push(c);
            prev_was_dash = false;
        } else if !prev_was_dash {
            result.push('-');
            prev_was_dash = true;
        }
    }
    result.trim_matches('-').to_string()
}

/// Format date prefix as MMDD using system time (matches TS `new Date()` approach).
fn format_date_prefix() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Convert epoch seconds to month/day (simplified: no timezone, UTC)
    // Days since epoch
    let days = (now / 86400) as i64;
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    // Adjust for local time offset (use seconds within day)
    let _ = if m <= 2 { yoe + 1 } else { yoe };
    format!("{:02}{:02}", m, d)
}

fn random_hex(bytes: usize) -> String {
    let mut rng = rand::thread_rng();
    (0..bytes).map(|_| format!("{:02x}", rng.r#gen::<u8>())).collect()
}

/// Generate a default base name: MMDD-XXXX
fn make_default_base_name() -> String {
    format!("{}-{}", format_date_prefix(), random_hex(2))
}

fn normalize_name_hint(hint: Option<&str>) -> Option<String> {
    let hint = hint?.trim();
    if hint.is_empty() {
        return None;
    }
    let slug = to_slug(hint);
    if slug.is_empty() { None } else { Some(slug) }
}

async fn branch_exists(repo_root: &Path, branch: &str) -> bool {
    run_git(&["show-ref", "--verify", &format!("refs/heads/{branch}")], repo_root)
        .await
        .is_ok()
}

/// Create a new git worktree for a session.
pub async fn create_worktree(
    base_path: &str,
    name_hint: Option<&str>,
) -> Result<WorktreeInfo, String> {
    let base = Path::new(base_path);
    let repo_root = resolve_repo_root(base).await
        .map_err(|e| format!("Path is not a Git repository: {e}"))?;

    let repo_root_path = Path::new(&repo_root);
    let repo_parent = repo_root_path.parent()
        .ok_or_else(|| "Cannot determine repository parent directory".to_string())?;
    let repo_name = repo_root_path.file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "Cannot determine repository name".to_string())?;

    let worktrees_root = repo_parent.join(format!("{repo_name}-worktrees"));
    std::fs::create_dir_all(&worktrees_root)
        .map_err(|e| format!("Failed to create worktrees directory: {e}"))?;

    let base_name = normalize_name_hint(name_hint)
        .unwrap_or_else(make_default_base_name);

    for attempt in 0..MAX_ATTEMPTS {
        let name = if attempt == 0 {
            base_name.clone()
        } else {
            format!("{}-{}", base_name, random_hex(2))
        };
        let branch = format!("hapi-{name}");
        let worktree_path = worktrees_root.join(&name);

        if worktree_path.exists() {
            continue;
        }
        if branch_exists(repo_root_path, &branch).await {
            continue;
        }

        let wt_str = worktree_path.to_string_lossy().to_string();
        run_git(&["worktree", "add", "-b", &branch, &wt_str], repo_root_path)
            .await
            .map_err(|e| format!("Failed to create worktree: {e}"))?;

        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        debug!(worktree_path = %wt_str, branch = %branch, "created worktree");

        return Ok(WorktreeInfo {
            base_path: repo_root,
            worktree_path: wt_str,
            branch,
            name,
            created_at,
        });
    }

    Err("Failed to create worktree after multiple attempts. Try again.".to_string())
}

/// Remove a git worktree.
pub async fn remove_worktree(repo_root: &str, worktree_path: &str) -> Result<(), String> {
    run_git(&["worktree", "remove", "--force", worktree_path], Path::new(repo_root)).await?;
    Ok(())
}
