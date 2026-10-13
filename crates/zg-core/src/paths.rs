//! Filesystem path helpers: home resolution, normalization, containment.

use std::path::{Path, PathBuf};

/// Directory name for this project's own state, both as the global home
/// under the user's home directory (`~/.zvec-grep-rs`) and as the
/// per-workspace index/state directory (`<root>/.zvec-grep-rs`). This is
/// the ONE canonical spelling; every other module must use this constant
/// instead of spelling the directory name itself. It is deliberately
/// distinct from the upstream TypeScript / upstream Rust state directory
/// ([`LEGACY_STATE_DIR_NAME`]) so the two implementations never share
/// config, indexes, locks, or grants.
pub const STATE_DIR_NAME: &str = ".zvec-grep-rs";

/// Legacy/upstream state directory name (`.zvec-grep`), used by the
/// upstream TypeScript implementation and upstream Rust rewrite. This
/// project never reads or writes state there; the name exists only so
/// scanners and watchers can hard-skip that foreign state and never index
/// it. Explicit user overrides (e.g. `$ZVEC_GREP_HOME`) may still point
/// wherever the user chooses.
pub const LEGACY_STATE_DIR_NAME: &str = ".zvec-grep";

/// Default global state directory: `$ZVEC_GREP_HOME` or `~/.zvec-grep-rs`.
pub fn default_home() -> PathBuf {
    if let Some(home) = std::env::var_os("ZVEC_GREP_HOME")
        && !home.is_empty()
    {
        return PathBuf::from(home);
    }
    let home_dir = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
    home_dir.join(STATE_DIR_NAME)
}

/// Global config file: `~/.zvec-grep-rs/config.json` (uses the real home
/// directory, not `$ZVEC_GREP_HOME`).
pub fn global_config_path() -> PathBuf {
    let home_dir = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
    home_dir.join(STATE_DIR_NAME).join("config.json")
}

/// Lexically absolute form of `path` (no symlink resolution, like `resolve`).
#[must_use]
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                result.push(component);
            }
        }
    }
    if result.as_os_str().is_empty() {
        PathBuf::from("/")
    } else {
        result
    }
}

/// Renders a path with `/` separators (for display and index keys).
#[must_use]
pub fn to_display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// True when `child` equals `parent` or lives underneath it.
#[must_use]
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
    #[test]
    fn state_dir_is_namespaced_from_upstream() {
        assert_eq!(STATE_DIR_NAME, ".zvec-grep-rs");
        assert_eq!(LEGACY_STATE_DIR_NAME, ".zvec-grep");
        assert_ne!(STATE_DIR_NAME, LEGACY_STATE_DIR_NAME);
    }

    #[test]
    fn global_config_lives_in_namespaced_home() {
        // `global_config_path` ignores `$ZVEC_GREP_HOME`, so this holds
        // regardless of the ambient environment: the fallback home must
        // be the namespaced directory, never the upstream one.
        let config = global_config_path();
        assert!(config.ends_with(Path::new(STATE_DIR_NAME).join("config.json")));
        assert!(!config.ends_with(Path::new(LEGACY_STATE_DIR_NAME).join("config.json")));
    }
}
