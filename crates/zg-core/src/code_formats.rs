//! Code format capability lists.

/// Formats with tree-sitter structural extraction.
pub const STRUCTURED_CODE_FORMATS: &[&str] = &[
    "c",
    "cpp",
    "go",
    "java",
    "javascript",
    "jsx",
    "python",
    "rust",
    "tsx",
    "typescript",
];

/// Component formats whose `<script>` blocks are extracted.
pub const COMPONENT_CODE_FORMATS: &[&str] = &["vue", "svelte"];

/// True when `format` has a tree-sitter grammar.
pub fn is_structured_code_format(format: &str) -> bool {
    STRUCTURED_CODE_FORMATS.contains(&format)
}

/// True when `format` is a component format (script-block extraction).
pub fn is_component_code_format(format: &str) -> bool {
    COMPONENT_CODE_FORMATS.contains(&format)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn membership() {
        assert!(is_structured_code_format("rust"));
        assert!(is_structured_code_format("tsx"));
        assert!(!is_structured_code_format("ruby"));
        assert!(is_component_code_format("vue"));
        assert!(!is_component_code_format("rust"));
    }
}
