//! Ripgrep file-type selection and ordered-glob matching.
//!
//! Divergence from TS: instead of shelling out to `rg --type-list`, type names
//! resolve through the `ignore` crate's default ripgrep type set. Ordered
//! globs keep the TS last-match-wins semantics.

use std::path::Path;

use ignore::types::{Types, TypesBuilder};

use crate::error::{EngineError, EngineResult};

/// zg name → ripgrep type name aliases (from `utils/file-selection.ts`).
const RIPGREP_FILE_TYPE_ALIASES: &[(&str, &str)] = &[
    ("bash", "sh"),
    ("cjs", "js"),
    ("cp", "cpp"),
    ("cc", "cpp"),
    ("cpp", "cpp"),
    ("cxx", "cpp"),
    ("hpp", "h"),
    ("hxx", "h"),
    ("hh", "h"),
    ("h", "h"),
    ("js", "js"),
    ("jsx", "js"),
    ("mjs", "js"),
    ("markdown", "md"),
    ("mdx", "md"),
    ("pyi", "py"),
    ("py", "py"),
    ("rb", "ruby"),
    ("rs", "rust"),
    ("ts", "ts"),
    ("tsx", "ts"),
    ("yml", "yaml"),
    ("zsh", "sh"),
];

/// Type-name `all` selects every file.
pub const ALL_FILE_TYPES: &str = "all";

/// Resolved file-type matcher: `(no include || included) && !excluded`.
#[derive(Debug, Clone)]
pub struct FileTypesMatcher {
    include: Option<Types>,
    exclude: Option<Types>,
    all: bool,
}

impl FileTypesMatcher {
    /// No type filtering at all.
    #[must_use]
    pub fn none() -> Self {
        Self {
            include: None,
            exclude: None,
            all: false,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.include.is_none() && self.exclude.is_none()
    }

    /// True when `path` passes the include/exclude type selection.
    #[must_use]
    pub fn matches(&self, path: &Path) -> bool {
        if self.is_empty() {
            return true;
        }
        if !self.all
            && let Some(include) = &self.include
            && !include.matched(path, false).is_whitelist()
        {
            return false;
        }
        if let Some(exclude) = &self.exclude
            && exclude.matched(path, false).is_ignore()
        {
            return false;
        }
        true
    }
}

/// Resolves requested type names (with aliases) into a matcher.
///
/// Unknown names produce `Unknown ripgrep file type: <name>`.
///
/// # Errors
///
/// Returns [`EngineError`] with `FILE_SELECTION.UNKNOWN_FILE_TYPE` when a name is not a known ripgrep type, or `FILE_SELECTION.TYPES_UNAVAILABLE` when the type set fails to build.
pub fn resolve_file_types(
    included: &[String],
    excluded: &[String],
) -> EngineResult<FileTypesMatcher> {
    let include = resolve_selection(included, true)?;
    let exclude = resolve_selection(excluded, false)?;
    let all = included
        .iter()
        .any(|n| n.trim().eq_ignore_ascii_case(ALL_FILE_TYPES));
    Ok(FileTypesMatcher {
        include,
        exclude,
        all,
    })
}

fn resolve_selection(names: &[String], selecting: bool) -> EngineResult<Option<Types>> {
    let filtered: Vec<String> = names
        .iter()
        .map(|n| n.trim().to_ascii_lowercase())
        .filter(|n| !n.is_empty())
        .collect();
    if filtered.is_empty() {
        return Ok(None);
    }
    let mut builder = TypesBuilder::new();
    builder.add_defaults();
    for name in &filtered {
        let resolved = alias_for(name).unwrap_or(name.as_str());
        if selecting {
            builder.select(resolved);
        } else {
            builder.negate(resolved);
        }
    }
    builder.build().map(Some).map_err(|error| match error {
        ignore::Error::UnrecognizedFileType(name) => {
            let raw = filtered
                .iter()
                .find(|candidate| alias_for(candidate).unwrap_or(candidate.as_str()) == name)
                .cloned()
                .unwrap_or(name);
            EngineError::new(
                codes::unknown_file_type(),
                format!("Unknown ripgrep file type: {raw}"),
            )
        }
        other @ (ignore::Error::Partial(_)
        | ignore::Error::WithLineNumber { .. }
        | ignore::Error::WithPath { .. }
        | ignore::Error::WithDepth { .. }
        | ignore::Error::Loop { .. }
        | ignore::Error::Io(_)
        | ignore::Error::Glob { .. }
        | ignore::Error::InvalidDefinition) => EngineError::new(
            codes::file_types_unavailable(),
            "Unable to load ripgrep file types",
        )
        .with_context(format!("error={other}")),
    })
}

fn alias_for(name: &str) -> Option<&str> {
    RIPGREP_FILE_TYPE_ALIASES
        .iter()
        .find(|(alias, _)| *alias == name)
        .map(|(_, target)| *target)
}

/// Ordered glob rules with last-match-wins semantics (`!pattern` negates).
#[derive(Debug, Clone, Default)]
pub struct OrderedGlobs {
    rules: Vec<GlobRule>,
}

#[derive(Debug, Clone)]
struct GlobRule {
    pattern: String,
    case_insensitive: bool,
    negated: bool,
}

impl OrderedGlobs {
    /// Builds from `globs` (case-sensitive) then `insensitive_globs`, trimming
    /// and dropping empty patterns.
    #[must_use]
    pub fn new(globs: &[String], insensitive_globs: &[String]) -> Self {
        let mut rules = Vec::with_capacity(globs.len() + insensitive_globs.len());
        let mut push = |raw: &str, case_insensitive: bool| {
            let trimmed = raw.trim();
            let (pattern, negated) = match trimmed.strip_prefix('!') {
                Some(rest) => (rest.trim(), true),
                None => (trimmed, false),
            };
            if !pattern.is_empty() {
                rules.push(GlobRule {
                    pattern: pattern.to_owned(),
                    case_insensitive,
                    negated,
                });
            }
        };
        for pattern in globs {
            push(pattern, false);
        }
        for pattern in insensitive_globs {
            push(pattern, true);
        }
        Self { rules }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Evaluates the rules in order; the last matching rule decides. With no
    /// positive rule present, paths start included; otherwise excluded until a
    /// positive rule matches.
    #[must_use]
    pub fn matches(&self, path: &str) -> bool {
        if self.rules.is_empty() {
            return true;
        }
        let has_positive = self.rules.iter().any(|r| !r.negated);
        let mut included = !has_positive;
        for rule in &self.rules {
            let matched = if rule.case_insensitive {
                crate::utils::glob::ripgrep_glob_matches_case_insensitive(&rule.pattern, path)
            } else {
                crate::utils::glob::ripgrep_glob_matches(&rule.pattern, path)
            };
            if matched {
                included = !rule.negated;
            }
        }
        included
    }
}

/// Combined path selection: ordered globs AND file-type filter.
#[derive(Debug, Clone)]
pub struct FileSelection {
    pub globs: OrderedGlobs,
    pub types: FileTypesMatcher,
}

impl FileSelection {
    #[must_use]
    pub fn matches(&self, path: &str) -> bool {
        self.globs.matches(path) && self.types.matches(Path::new(path))
    }
}

mod codes {
    use crate::error::EngineErrorCode;

    pub fn file_types_unavailable() -> EngineErrorCode {
        EngineErrorCode::FileSelectionTypesUnavailable
    }

    pub fn unknown_file_type() -> EngineErrorCode {
        EngineErrorCode::FileSelectionUnknownFileType
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(globs: &[&str], insensitive: &[&str]) -> OrderedGlobs {
        let g: Vec<String> = globs.iter().map(|s| s.to_string()).collect();
        let i: Vec<String> = insensitive.iter().map(|s| s.to_string()).collect();
        OrderedGlobs::new(&g, &i)
    }

    #[test]
    fn empty_selection_matches_everything() {
        assert!(sel(&[], &[]).matches("anything/x.rs"));
    }

    #[test]
    fn positive_globs_exclude_unmatched() {
        let rules = sel(&["*.rs", "src/**"], &[]);
        assert!(rules.matches("a.rs"));
        assert!(rules.matches("src/b/c.txt"));
        assert!(!rules.matches("other.txt"));
    }

    #[test]
    fn negation_after_match_wins() {
        let rules = sel(&["*.rs", "!skip_*.rs"], &[]);
        assert!(rules.matches("good.rs"));
        assert!(!rules.matches("skip_me.rs"));
    }

    #[test]
    fn negation_only_starts_included() {
        let rules = sel(&["!*.lock"], &[]);
        assert!(rules.matches("main.rs"));
        assert!(!rules.matches("cargo.lock"));
    }

    #[test]
    fn case_insensitive_globs() {
        let rules = sel(&[], &["*.MD"]);
        assert!(rules.matches("readme.md"));
        assert!(rules.matches("README.MD"));
        assert!(!rules.matches("x.txt"));
    }

    #[test]
    fn file_types_resolve_and_match() {
        let included = vec!["rust".to_owned()];
        let matcher = resolve_file_types(&included, &[]).expect("resolve");
        assert!(matcher.matches(Path::new("src/main.rs")));
        assert!(!matcher.matches(Path::new("src/main.py")));
    }

    #[test]
    fn aliases_resolve() {
        let included = vec!["mdx".to_owned()];
        let matcher = resolve_file_types(&included, &[]).expect("resolve");
        assert!(matcher.matches(Path::new("docs/x.mdx")));
        assert!(!matcher.matches(Path::new("src/x.rs")));
    }

    #[test]
    fn excluded_types_reject() {
        let excluded = vec!["rust".to_owned()];
        let matcher = resolve_file_types(&[], &excluded).expect("resolve");
        assert!(!matcher.matches(Path::new("a.rs")));
        assert!(matcher.matches(Path::new("a.py")));
    }

    #[test]
    fn unknown_type_rejected() {
        let error = resolve_file_types(&["nosuchtype".to_owned()], &[]);
        assert!(error.is_err());
        let message = error.err().map(|e| e.message().to_owned());
        assert_eq!(
            message.as_deref(),
            Some("Unknown ripgrep file type: nosuchtype")
        );
    }

    #[test]
    fn all_matches_everything() {
        let included = vec!["all".to_owned()];
        let matcher = resolve_file_types(&included, &[]).expect("resolve");
        assert!(matcher.matches(Path::new("a.rs")));
        assert!(matcher.matches(Path::new("b.xyz")));
    }
}
