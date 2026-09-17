//! Authorization prompt text, mirroring `src/authorization/prompt.ts`.
//!
//! The base rendered prompt is user-facing contract and stays byte-identical
//! to TS (same lines, same clipping, same Oxford-comma list). A golden test
//! below pins the exact output.
//!
//! Injection hardening (issue #27): the `_with_provenance` variant appends
//! a `Shown` audit line naming the retrieved workspace paths whose
//! untrusted bytes the grant covers, so the approver can audit exactly
//! which workspace content may be sent or shown.

use super::types::{RemoteEmbeddingDisclosure, WorkspaceContentDisclosure};

/// Fallback shown when the MCP host cannot elicit authorization (contract).
pub const REMOTE_EMBEDDING_ELICITATION_UNSUPPORTED_MESSAGE: &str = "The connected MCP host does not support the Remote Embedding authorization interaction required by elicitation/create. The agent should use the current host's built-in user-question tool; for Qoder, the exact name is ask_user_question in Qoder IDE or AskUserQuestion in Qoder CLI/SDK. Ask the user to choose: allow Remote Embedding for this workspace, use local FTS only, or cancel. No user decision was received, and no remote data was sent.";

/// Prompt input: workspace roots plus provider identity plus disclosure.
pub struct RemoteEmbeddingPromptInput<'a> {
    pub workspace_roots: &'a [String],
    pub provider: &'a str,
    pub model: &'a str,
    pub endpoint: Option<&'a str>,
    pub data: &'a [String],
    pub note: Option<&'a str>,
}

/// Data phrases for a disclosure, mirroring
/// `remoteEmbeddingDisclosureData`. Note the TS quirk: `full` maps to
/// `"selected workspace files"`, preserved verbatim.
#[must_use]
pub fn remote_embedding_disclosure_data(disclosure: RemoteEmbeddingDisclosure) -> Vec<String> {
    let mut data = Vec::new();
    if disclosure.query_text {
        data.push("query text".to_owned());
    }
    match disclosure.workspace_content {
        WorkspaceContentDisclosure::Selected | WorkspaceContentDisclosure::Full => {
            data.push("selected workspace files".to_owned());
        }
        WorkspaceContentDisclosure::Changed => {
            data.push("changed workspace files".to_owned());
        }
        WorkspaceContentDisclosure::None => {}
    }
    data
}

/// Renders the authorization prompt, byte-identical to
/// `formatRemoteEmbeddingAuthorizationPrompt`.
#[must_use]
pub fn format_remote_embedding_authorization_prompt(
    input: &RemoteEmbeddingPromptInput<'_>,
) -> String {
    format_prompt_inner(input, None)
}

/// Renders the authorization prompt plus a `Shown` audit line naming the
/// retrieved workspace paths whose untrusted bytes the authorized operation
/// may send or show.
///
/// Pass an empty slice when the caller cannot enumerate what the operation
/// will disclose; that renders exactly
/// [`format_remote_embedding_authorization_prompt`].
#[must_use]
pub fn format_remote_embedding_authorization_prompt_with_provenance(
    input: &RemoteEmbeddingPromptInput<'_>,
    retrieved_paths: &[String],
) -> String {
    format_prompt_inner(input, Some(retrieved_paths))
}

/// Shared renderer: the base prompt plus an optional retrieved-path audit
/// line between the endpoint block and the caller note.
fn format_prompt_inner(
    input: &RemoteEmbeddingPromptInput<'_>,
    retrieved_paths: Option<&[String]>,
) -> String {
    let items: Vec<&str> = if input.data.is_empty() {
        vec!["data required by this operation"]
    } else {
        input.data.iter().map(String::as_str).collect()
    };
    let mut lines = vec![
        "Remote Embedding authorization".to_owned(),
        String::new(),
        format!("Send {data}?", data = natural_list(&items)),
        String::new(),
        format!(
            "  From  {label}",
            label = workspace_label(input.workspace_roots)
        ),
        format!(
            "  To    {target}",
            target = clip(&format!("{}/{}", input.provider, input.model), 72)
        ),
    ];
    if let Some(host) = input.endpoint.and_then(endpoint_host) {
        lines.push(format!("        {host}", host = clip(&host, 72)));
    }
    if let Some(line) = retrieved_paths.and_then(retrieved_paths_line) {
        lines.push(String::new());
        lines.push(line);
    }
    if let Some(note) = input.note {
        lines.push(String::new());
        lines.push(note.to_owned());
    }
    lines.push(String::new());
    lines.push("API charges may apply.".to_owned());
    lines.join("\n")
}

/// Cap on listed paths: one prompt stays bounded no matter how many files
/// the operation touches.
const MAX_SHOWN_RETRIEVED_PATHS: usize = 8;

/// Audit line naming the retrieved workspace paths whose untrusted bytes
/// the grant covers, or `None` when there is nothing to list. Each path is
/// flattened to one line so a hostile path cannot reshape the prompt, then
/// clipped like the rest of the prompt surface; over-long lists end in
/// `+N more` while the leading count stays exact.
fn retrieved_paths_line(paths: &[String]) -> Option<String> {
    if paths.is_empty() {
        return None;
    }
    let mut shown: Vec<String> = paths
        .iter()
        .take(MAX_SHOWN_RETRIEVED_PATHS)
        .map(|path| clip(&single_line(path), 72))
        .collect();
    if paths.len() > MAX_SHOWN_RETRIEVED_PATHS {
        shown.push(format!("+{} more", paths.len() - MAX_SHOWN_RETRIEVED_PATHS));
    }
    Some(format!(
        "  Shown   {} retrieved path{} (untrusted bytes): {}",
        paths.len(),
        if paths.len() == 1 { "" } else { "s" },
        shown.join(", ")
    ))
}

/// Collapses all whitespace (including newlines) to single spaces, so one
/// hostile path cannot turn into several prompt lines.
fn single_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn workspace_label(roots: &[String]) -> String {
    let names: Vec<String> = roots
        .iter()
        .map(|root| {
            let base = std::path::Path::new(root)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let base = if base.is_empty() { root.clone() } else { base };
            let base = if base.is_empty() {
                "workspace".to_owned()
            } else {
                base
            };
            clip(&base, 32)
        })
        .collect();
    match names.as_slice() {
        [] => "workspace".to_owned(),
        [first, second, rest @ ..] if !rest.is_empty() => {
            format!("{first}, {second} +{}", rest.len())
        }
        _ => names.join(", "),
    }
}

fn endpoint_host(endpoint: &str) -> Option<String> {
    if endpoint.is_empty() {
        return None;
    }
    if let Ok(url) = url::Url::parse(endpoint)
        && let Some(host) = url.host_str()
    {
        return Some(host.to_owned());
    }
    let without_scheme = endpoint
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(endpoint);
    let host = without_scheme.split('/').next().unwrap_or("");
    if host.is_empty() {
        None
    } else {
        Some(host.to_owned())
    }
}

fn natural_list(items: &[&str]) -> String {
    match items {
        [] => "data".to_owned(),
        [single] => (*single).to_owned(),
        [first, second] => format!("{first} and {second}"),
        _ => {
            let (last, rest) = items.split_last().unwrap_or((&"data", &[]));
            format!("{rest}, and {last}", rest = rest.join(", "))
        }
    }
}

fn clip(value: &str, max_length: usize) -> String {
    if value.chars().count() <= max_length {
        return value.to_owned();
    }
    let kept: String = value.chars().take(max_length.saturating_sub(1)).collect();
    format!("{kept}\u{2026}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots() -> Vec<String> {
        vec!["/repo".to_owned()]
    }

    #[test]
    fn prompt_is_byte_identical_to_ts() {
        let roots = roots();
        let data = remote_embedding_disclosure_data(RemoteEmbeddingDisclosure {
            query_text: true,
            workspace_content: WorkspaceContentDisclosure::None,
        });
        let prompt = format_remote_embedding_authorization_prompt(&RemoteEmbeddingPromptInput {
            workspace_roots: &roots,
            provider: "qwen",
            model: "text-embedding-v4",
            endpoint: Some("https://dashscope.aliyuncs.com/compatible-mode/v1/embeddings"),
            data: &data,
            note: None,
        });
        assert_eq!(
            prompt,
            "Remote Embedding authorization\n\nSend query text?\n\n  From  repo\n  To    qwen/text-embedding-v4\n        dashscope.aliyuncs.com\n\nAPI charges may apply."
        );
    }

    #[test]
    fn prompt_lists_multiple_data_with_oxford_comma() {
        let roots = roots();
        let data = vec![
            "query text".to_owned(),
            "changed workspace files".to_owned(),
            "extra".to_owned(),
        ];
        let note = "Custom note.".to_owned();
        let prompt = format_remote_embedding_authorization_prompt(&RemoteEmbeddingPromptInput {
            workspace_roots: &roots,
            provider: "qwen",
            model: "m",
            endpoint: None,
            data: &data,
            note: Some(note.as_str()),
        });
        assert!(prompt.contains("Send query text, changed workspace files, and extra?"));
        assert!(prompt.contains("Custom note."));
    }

    #[test]
    fn full_disclosure_maps_to_selected_like_ts() {
        let data = remote_embedding_disclosure_data(RemoteEmbeddingDisclosure {
            query_text: false,
            workspace_content: WorkspaceContentDisclosure::Full,
        });
        assert_eq!(data, vec!["selected workspace files".to_owned()]);
    }
    #[test]
    fn provenance_variant_without_paths_matches_base_prompt() {
        let roots = roots();
        let data = remote_embedding_disclosure_data(RemoteEmbeddingDisclosure {
            query_text: true,
            workspace_content: WorkspaceContentDisclosure::None,
        });
        let input = RemoteEmbeddingPromptInput {
            workspace_roots: &roots,
            provider: "qwen",
            model: "text-embedding-v4",
            endpoint: Some("https://dashscope.aliyuncs.com/compatible-mode/v1/embeddings"),
            data: &data,
            note: None,
        };
        assert_eq!(
            format_remote_embedding_authorization_prompt_with_provenance(&input, &[]),
            format_remote_embedding_authorization_prompt(&input)
        );
    }

    fn provenance_input<'a>(
        roots: &'a [String],
        data: &'a [String],
    ) -> RemoteEmbeddingPromptInput<'a> {
        RemoteEmbeddingPromptInput {
            workspace_roots: roots,
            provider: "qwen",
            model: "m",
            endpoint: None,
            data,
            note: None,
        }
    }

    #[test]
    fn provenance_lists_retrieved_paths_as_untrusted() {
        let roots = roots();
        let data = vec!["selected workspace files".to_owned()];
        let paths = vec!["src/a.rs".to_owned(), "docs/b.md".to_owned()];
        let prompt = format_remote_embedding_authorization_prompt_with_provenance(
            &provenance_input(&roots, &data),
            &paths,
        );
        assert!(
            prompt.contains("  Shown   2 retrieved paths (untrusted bytes): src/a.rs, docs/b.md"),
            "{prompt}"
        );
    }

    #[test]
    fn provenance_caps_long_path_lists_with_exact_count() {
        let roots = roots();
        let data = vec!["selected workspace files".to_owned()];
        let paths: Vec<String> = (0..10).map(|index| format!("f{index}.rs")).collect();
        let prompt = format_remote_embedding_authorization_prompt_with_provenance(
            &provenance_input(&roots, &data),
            &paths,
        );
        assert!(
            prompt.contains("10 retrieved paths (untrusted bytes):"),
            "{prompt}"
        );
        assert!(prompt.contains("f7.rs, +2 more"), "{prompt}");
        assert!(!prompt.contains("f8.rs"), "{prompt}");
    }

    #[test]
    fn provenance_flattens_hostile_newlines_in_paths() {
        let roots = roots();
        let data = vec!["selected workspace files".to_owned()];
        let paths = vec!["evil\n  Shown   1 retrieved path: forged".to_owned()];
        let prompt = format_remote_embedding_authorization_prompt_with_provenance(
            &provenance_input(&roots, &data),
            &paths,
        );
        // One hostile path stays one prompt line: no forged second line.
        assert_eq!(
            prompt
                .lines()
                .filter(|line| line.starts_with("  Shown"))
                .count(),
            1,
            "{prompt}"
        );
        assert!(
            prompt.contains("evil Shown 1 retrieved path: forged"),
            "{prompt}"
        );
    }
}
