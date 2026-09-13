//! Filesystem path helpers: home resolution, normalization, containment.

use std::path::{Path, PathBuf};

/// Default global state directory: `$ZVEC_GREP_HOME` or `~/.zvec-grep`.
pub fn default_home() -> PathBuf {
    if let Some(home) = std::env::var_os("ZVEC_GREP_HOME")
        && !home.is_empty()
    {
        return PathBuf::from(home);
    }
    let home_dir = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home_dir.join(".zvec-grep")
}

/// Global config file: `~/.zvec-grep/config.json` (mirrors TS
/// `globalConfigPath`; uses the real home directory, not `$ZVEC_GREP_HOME`).
pub fn global_config_path() -> PathBuf {
    let home_dir = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home_dir.join(".zvec-grep").join("config.json")
}

/// Lexically absolute form of `path` (no symlink resolution, like `resolve`).
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other),
        }
    }
    if result.as_os_str().is_empty() {
        PathBuf::from("/")
    } else {
        result
    }
}

/// Renders a path with `/` separators (for display and index keys).
pub fn to_display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// True when `child` equals `parent` or lives underneath it.
pub fn is_path_inside(parent: &Path, child: &Path) -> bool {
    if child == parent {
        return true;
    }
    child.starts_with(parent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_collapses_dots() {
        assert_eq!(
            normalize_path(Path::new("/a/./b/../c")),
            PathBuf::from("/a/c")
        );
    }

    #[test]
    fn containment_checks() {
        let parent = Path::new("/a/b");
        assert!(is_path_inside(parent, Path::new("/a/b")));
        assert!(is_path_inside(parent, Path::new("/a/b/c/d.txt")));
        assert!(!is_path_inside(parent, Path::new("/a/bc")));
    }

    #[test]
    fn display_uses_forward_slashes() {
        assert_eq!(to_display_path(Path::new("/a/b")), "/a/b");
    }
}
