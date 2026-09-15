//! Extension/name based file type detection.
//!
//! Ports `engine/file-type.ts`: named files win, then extension maps in
//! code → data → text → image order, binary extensions are rejected, and every
//! unknown non-binary extension falls back to plain text named after itself.

use std::collections::BTreeMap;
use std::path::Path;

use crate::types::{FileFormat, FileKind};

/// A detected type: coarse kind plus specific format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileType {
    pub kind: FileKind,
    pub format: FileFormat,
}

struct NamedTypeEntry {
    patterns: &'static [&'static str],
    kind: FileKind,
    format: &'static str,
}

const CODE_EXTENSIONS: &[(&str, &str)] = &[
    ("c", "c"),
    ("cc", "cpp"),
    ("cpp", "cpp"),
    ("cxx", "cpp"),
    ("h", "cpp"),
    ("hpp", "cpp"),
    ("go", "go"),
    ("java", "java"),
    ("js", "javascript"),
    ("mjs", "javascript"),
    ("cjs", "javascript"),
    ("jsx", "jsx"),
    ("ts", "typescript"),
    ("tsx", "tsx"),
    ("py", "python"),
    ("rs", "rust"),
    ("rb", "ruby"),
    ("php", "php"),
    ("swift", "swift"),
    ("kt", "kotlin"),
    ("kts", "kotlin"),
    ("cs", "csharp"),
    ("scala", "scala"),
    ("sh", "bash"),
    ("bash", "bash"),
    ("zsh", "bash"),
    ("sql", "sql"),
    ("css", "css"),
    ("scss", "scss"),
    ("less", "less"),
    ("vue", "vue"),
    ("svelte", "svelte"),
    ("vb", "vb"),
];

const DATA_EXTENSIONS: &[(&str, &str)] = &[
    ("csv", "csv"),
    ("json", "json"),
    ("jsonc", "json"),
    ("toml", "toml"),
    ("yaml", "yaml"),
    ("yml", "yaml"),
];

const TEXT_EXTENSIONS: &[(&str, &str)] = &[
    ("md", "markdown"),
    ("mdx", "markdown"),
    ("rst", "rst"),
    ("txt", "text"),
    ("html", "html"),
    ("htm", "html"),
    ("xml", "xml"),
];

const IMAGE_EXTENSIONS: &[(&str, &str)] = &[
    ("gif", "gif"),
    ("jpeg", "jpeg"),
    ("jpg", "jpeg"),
    ("png", "png"),
    ("webp", "webp"),
];

const NAMED_ENTRIES: &[NamedTypeEntry] = &[
    NamedTypeEntry {
        patterns: &["Dockerfile"],
        kind: FileKind::Code,
        format: "dockerfile",
    },
    NamedTypeEntry {
        patterns: &["Makefile"],
        kind: FileKind::Code,
        format: "makefile",
    },
];

/// Extension groups treated as binary (never indexed).
pub const BINARY_EXTENSION_GROUPS: &[(&str, &[&str])] = &[
    ("archives", &["zip", "tar", "gz", "bz2", "xz", "7z", "rar"]),
    (
        "compiled",
        &[
            "exe", "dll", "dylib", "so", "a", "o", "obj", "wasm", "class", "jar",
        ],
    ),
    (
        "documents",
        &["pdf", "doc", "docx", "ppt", "pptx", "xls", "xlsx"],
    ),
    ("media", &["mp3", "mp4", "mov", "avi", "mkv"]),
    ("databases", &["db", "sqlite"]),
];

fn binary_extensions() -> std::collections::HashSet<&'static str> {
    BINARY_EXTENSION_GROUPS
        .iter()
        .flat_map(|(_, exts)| exts.iter().copied())
        .collect()
}

/// Detects the type of `path` by basename and extension.
///
/// Returns `None` for known-binary extensions. Unknown non-binary extensions
/// fall back to `text` with the extension as format (`text` when absent).
pub fn detect_file_type(path: &Path) -> Option<FileType> {
    if let Some(basename) = path.file_name().and_then(|n| n.to_str()) {
        for entry in NAMED_ENTRIES {
            if entry.patterns.contains(&basename) {
                return Some(FileType {
                    kind: entry.kind,
                    format: FileFormat::parse(entry.format),
                });
            }
        }
    }
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    let Some(extension) = extension else {
        return Some(FileType {
            kind: FileKind::Text,
            format: FileFormat::parse("text"),
        });
    };
    for (kind, table) in [
        (FileKind::Code, CODE_EXTENSIONS),
        (FileKind::Data, DATA_EXTENSIONS),
        (FileKind::Text, TEXT_EXTENSIONS),
        (FileKind::Image, IMAGE_EXTENSIONS),
    ] {
        for (ext, format) in table {
            if *ext == extension {
                return Some(FileType {
                    kind,
                    format: FileFormat::parse(format),
                });
            }
        }
    }
    if binary_extensions().contains(extension.as_str()) {
        return None;
    }
    Some(FileType {
        kind: FileKind::Text,
        format: FileFormat::parse(extension),
    })
}

/// Entry of [`list_recognized_file_types`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecognizedFileType {
    pub kind: FileKind,
    pub format: FileFormat,
    pub patterns: Vec<String>,
}

/// All recognized types with the patterns that produce them, deduplicated by
/// `kind:format`.
#[must_use]
pub fn list_recognized_file_types() -> Vec<RecognizedFileType> {
    let mut grouped: BTreeMap<(FileKind, &'static str), Vec<String>> = BTreeMap::new();
    for (kind, table) in [
        (FileKind::Code, CODE_EXTENSIONS),
        (FileKind::Data, DATA_EXTENSIONS),
        (FileKind::Text, TEXT_EXTENSIONS),
        (FileKind::Image, IMAGE_EXTENSIONS),
    ] {
        for (ext, format) in table {
            grouped
                .entry((kind, format))
                .or_default()
                .push(format!(".{ext}"));
        }
    }
    for entry in NAMED_ENTRIES {
        for pattern in entry.patterns {
            grouped
                .entry((entry.kind, entry.format))
                .or_default()
                .push((*pattern).to_owned());
        }
    }
    grouped
        .into_iter()
        .map(|((kind, format), mut patterns)| {
            patterns.sort();
            RecognizedFileType {
                kind,
                format: FileFormat::parse(format),
                patterns,
            }
        })
        .collect()
}

/// Copies of the binary extension groups.
#[must_use]
pub fn list_known_binary_extension_groups() -> Vec<(String, Vec<String>)> {
    BINARY_EXTENSION_GROUPS
        .iter()
        .map(|(name, exts)| {
            (
                (*name).to_owned(),
                exts.iter().map(|e| format!(".{e}")).collect(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detect(path: &str) -> Option<(FileKind, String)> {
        detect_file_type(Path::new(path)).map(|t| (t.kind, t.format.as_str().to_owned()))
    }

    #[test]
    fn named_files_win() {
        assert_eq!(
            detect("src/Dockerfile"),
            Some((FileKind::Code, "dockerfile".to_owned()))
        );
        assert_eq!(
            detect("Makefile"),
            Some((FileKind::Code, "makefile".to_owned()))
        );
    }

    #[test]
    fn code_extensions() {
        assert_eq!(detect("main.rs"), Some((FileKind::Code, "rust".to_owned())));
        assert_eq!(
            detect("lib.TS"),
            Some((FileKind::Code, "typescript".to_owned()))
        );
        assert_eq!(detect("a.hpp"), Some((FileKind::Code, "cpp".to_owned())));
        assert_eq!(detect("app.vue"), Some((FileKind::Code, "vue".to_owned())));
        assert_eq!(detect("a.vb"), Some((FileKind::Code, "vb".to_owned())));
        assert_eq!(detect("A.VB"), Some((FileKind::Code, "vb".to_owned())));
    }

    #[test]
    fn data_text_image_extensions() {
        assert_eq!(detect("x.csv"), Some((FileKind::Data, "csv".to_owned())));
        assert_eq!(
            detect("README.md"),
            Some((FileKind::Text, "markdown".to_owned()))
        );
        assert_eq!(detect("i.png"), Some((FileKind::Image, "png".to_owned())));
    }

    #[test]
    fn binary_rejected() {
        for path in ["a.zip", "b.so", "c.pdf", "d.mp4", "e.sqlite", "f.wasm"] {
            assert_eq!(detect(path), None, "{path} should be binary");
        }
    }

    #[test]
    fn unknown_extension_falls_back_to_text() {
        assert_eq!(
            detect("notes.log"),
            Some((FileKind::Text, "log".to_owned()))
        );
        assert_eq!(detect("noext"), Some((FileKind::Text, "text".to_owned())));
    }

    #[test]
    fn recognized_listing_dedupes() {
        let types = list_recognized_file_types();
        let cpp = types
            .iter()
            .find(|t| t.format.as_str() == "cpp")
            .expect("cpp entry");
        assert!(cpp.patterns.contains(&".cc".to_owned()));
        assert!(cpp.patterns.contains(&".hpp".to_owned()));
        assert_eq!(
            list_known_binary_extension_groups().len(),
            BINARY_EXTENSION_GROUPS.len()
        );
    }
}
