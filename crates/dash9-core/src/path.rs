//! Workspace-relative path enforcement for `dash save`/`dash open`.
//! See `docs/specs/assist.md` Section C.2: nothing previously stopped
//! either verb's path argument from resolving outside the workspace
//! root — fine for a trusted human typing at a terminal, not
//! acceptable once a less-trusted origin (an LLM) can propose the
//! same command. This check applies to every command source, human
//! or automated; it is not assist-specific.

use std::path::{Component, Path, PathBuf};

use crate::error::{CommandError, ErrorCode};

fn workspace_escape(candidate: &str) -> CommandError {
    CommandError::new(
        ErrorCode::E107,
        format!("path \"{candidate}\" resolves outside the workspace root"),
        None,
    )
}

/// Rejects a `dash save`/`dash open` path that would resolve outside
/// `workspace_root`: an absolute path, a `..` component, or (when
/// enough of the path already exists to check) a symlink escape.
///
/// A brand-new `dash save` target has no existing file to
/// canonicalize; in that case this falls back to canonicalizing the
/// nearest existing ancestor directory. If no ancestor exists either,
/// the lexical checks (absolute path, `..` component) are all that's
/// possible — still enough to catch the cases that matter in
/// practice.
pub fn validate_workspace_relative_path(
    workspace_root: &Path,
    candidate: &str,
) -> Result<PathBuf, CommandError> {
    let candidate_path = Path::new(candidate);
    if candidate_path.is_absolute() {
        return Err(workspace_escape(candidate));
    }
    if candidate_path
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(workspace_escape(candidate));
    }

    let joined = workspace_root.join(candidate_path);

    let existing_ancestor = if joined.exists() {
        Some(joined.clone())
    } else {
        joined
            .ancestors()
            .skip(1)
            .find(|p| p.exists())
            .map(Path::to_path_buf)
    };

    if let Some(existing_ancestor) = existing_ancestor {
        let canonical_root = workspace_root
            .canonicalize()
            .map_err(|_| workspace_escape(candidate))?;
        let canonical_existing = existing_ancestor
            .canonicalize()
            .map_err(|_| workspace_escape(candidate))?;
        if !canonical_existing.starts_with(&canonical_root) {
            return Err(workspace_escape(candidate));
        }
    }

    Ok(joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_path_is_rejected() {
        let root = std::env::temp_dir();
        let err = validate_workspace_relative_path(&root, "/etc/passwd").unwrap_err();
        assert_eq!(err.code, ErrorCode::E107);
    }

    #[test]
    fn parent_dir_traversal_is_rejected() {
        let root = std::env::temp_dir();
        let err = validate_workspace_relative_path(&root, "../outside.toml").unwrap_err();
        assert_eq!(err.code, ErrorCode::E107);
    }

    #[test]
    fn traversal_disguised_mid_path_is_rejected() {
        let root = std::env::temp_dir();
        let err =
            validate_workspace_relative_path(&root, "dashboards/../../outside.toml").unwrap_err();
        assert_eq!(err.code, ErrorCode::E107);
    }

    #[test]
    fn plain_relative_path_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let result = validate_workspace_relative_path(dir.path(), "examples/demo.toml").unwrap();
        assert_eq!(result, dir.path().join("examples/demo.toml"));
    }

    #[test]
    fn existing_file_inside_workspace_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("dashboard.toml"), "").unwrap();
        assert!(validate_workspace_relative_path(dir.path(), "dashboard.toml").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escaping_the_workspace_is_rejected() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.toml"), "").unwrap();
        let link = workspace.path().join("escape");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();

        let err =
            validate_workspace_relative_path(workspace.path(), "escape/secret.toml").unwrap_err();
        assert_eq!(err.code, ErrorCode::E107);
    }
}
