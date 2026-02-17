use std::path::{Component, Path, PathBuf};

/// Normalize a path by resolving `.` and `..` components without touching the filesystem.
fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Only pop if we have a normal component to pop
                if matches!(components.last(), Some(Component::Normal(_))) {
                    components.pop();
                } else {
                    components.push(component);
                }
            }
            _ => components.push(component),
        }
    }
    components.iter().collect()
}

/// Validate that `target_path` resolves to a location within `working_directory`.
///
/// Returns the resolved absolute path on success, or an error message on failure.
pub fn validate_path(target_path: &str, working_directory: &str) -> Result<PathBuf, String> {
    let working_dir = Path::new(working_directory);
    let resolved_target = normalize_path(&working_dir.join(target_path));
    let resolved_working = normalize_path(working_dir);

    let target_str = resolved_target.to_string_lossy();
    let working_str = resolved_working.to_string_lossy();

    // Exact match is allowed (e.g. listing the working directory itself)
    if target_str == working_str {
        return Ok(resolved_target);
    }

    // Target must be under working_directory + separator
    let prefix = if working_str.ends_with('/') {
        working_str.to_string()
    } else {
        format!("{}/", working_str)
    };

    if target_str.starts_with(&prefix) {
        Ok(resolved_target)
    } else {
        Err(format!(
            "Access denied: Path '{}' is outside the working directory",
            target_path
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_path_within_working_dir() {
        let result = validate_path("src/main.rs", "/home/user/project");
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            PathBuf::from("/home/user/project/src/main.rs")
        );
    }

    #[test]
    fn allows_working_dir_itself() {
        let result = validate_path(".", "/home/user/project");
        assert!(result.is_ok());
    }

    #[test]
    fn rejects_path_traversal() {
        let result = validate_path("../../etc/passwd", "/home/user/project");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Access denied"));
    }

    #[test]
    fn rejects_absolute_path_outside() {
        let result = validate_path("/etc/passwd", "/home/user/project");
        assert!(result.is_err());
    }

    #[test]
    fn allows_absolute_path_inside() {
        let result = validate_path("/home/user/project/file.txt", "/home/user/project");
        // absolute path join: /home/user/project + /home/user/project/file.txt
        // Path::join with an absolute second path replaces the first
        // So this resolves to /home/user/project/file.txt which IS inside
        assert!(result.is_ok());
    }
}
