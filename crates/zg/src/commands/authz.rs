//! Remote-embedding authorization prompts shared by `query` and `index`.
//!
//! Mirrors `authorizeCliPlan` including the verbatim texts: existing
//! grant, `--allow-remote` once, TTY prompt, or the non-TTY refusal.

use std::path::Path;

use zg_core::authorization::{
    RemoteEmbeddingAuthorizationManager, RemoteEmbeddingPermit, RemoteEmbeddingPromptInput,
    RemoteEmbeddingScope, format_remote_embedding_authorization_prompt,
    remote_embedding_disclosure_data,
};

use crate::error::CliError;

/// Search-side authorization outcome: the permit plus whether the user
/// chose FTS-only fallback.
pub(crate) struct SearchPermit {
    pub(crate) permit: Option<RemoteEmbeddingPermit>,
    pub(crate) fts_fallback: bool,
}

impl SearchPermit {
    pub(crate) fn none() -> Self {
        Self {
            permit: None,
            fts_fallback: false,
        }
    }
}

/// Resolves one authorization plan to a permit.
///
/// `allow_remote` is `--allow-remote` (grant once without prompting);
/// `offer_fts` adds the "Use FTS only" option (query only).
/// The `match` on the trimmed answer needs its wildcard: free-form
/// stdin input is not an enum, so exhaustiveness cannot apply.
pub(crate) async fn authorize_plan(
    plan: &zg_core::authorization::RemoteEmbeddingPlan,
    allow_remote: bool,
    offer_fts: bool,
) -> Result<SearchPermit, CliError> {
    let manager = RemoteEmbeddingAuthorizationManager::new();
    if let Some(permit) = manager.existing_workspace_permit(&plan.target)? {
        return Ok(SearchPermit {
            permit: Some(permit),
            fts_fallback: false,
        });
    }
    if allow_remote {
        let permit = manager.grant(&plan.target, RemoteEmbeddingScope::Once)?;
        return Ok(SearchPermit {
            permit: Some(permit),
            fts_fallback: false,
        });
    }
    if !std::io::IsTerminal::is_terminal(&std::io::stdin())
        || !std::io::IsTerminal::is_terminal(&std::io::stderr())
    {
        return Err(CliError::AuthorizationRequired {
            message: [
                "Remote Embedding authorization is required.",
                "Re-run with --allow-remote, or grant Workspace authorization:",
                "  zg auth grant --capability embedding --scope workspace",
            ]
            .join("\n"),
        });
    }
    let data = remote_embedding_disclosure_data(plan.disclosure);
    eprintln!(
        "{}",
        format_remote_embedding_authorization_prompt(&RemoteEmbeddingPromptInput {
            workspace_roots: &plan.target.workspace_roots,
            provider: &plan.target.provider,
            model: &plan.target.model,
            endpoint: Some(plan.target.endpoint.as_str()),
            data: &data,
            note: None,
        })
    );
    eprintln!();
    eprintln!("1. Allow once");
    eprintln!("2. Allow for this workspace");
    if offer_fts {
        eprintln!("3. Use FTS only");
    }
    let cancel = if offer_fts { 4 } else { 3 };
    eprintln!("{cancel}. Cancel");
    let answer = read_choice(&format!("Choose [1-{cancel}]: "))?;
    match answer.trim() {
        "1" => Ok(SearchPermit {
            permit: Some(manager.grant(&plan.target, RemoteEmbeddingScope::Once)?),
            fts_fallback: false,
        }),
        "2" => Ok(SearchPermit {
            permit: Some(manager.grant(&plan.target, RemoteEmbeddingScope::Workspace)?),
            fts_fallback: false,
        }),
        "3" if offer_fts => Ok(SearchPermit {
            permit: None,
            fts_fallback: true,
        }),
        _ => Err(CliError::AuthorizationDeclined {
            message: "Remote Embedding authorization was declined. No remote data was sent."
                .to_owned(),
        }),
    }
}

pub(crate) fn read_choice(prompt: &str) -> Result<String, CliError> {
    use std::io::Write;
    let mut stderr = std::io::stderr().lock();
    stderr
        .write_all(prompt.as_bytes())
        .and_then(|()| stderr.flush())
        .map_err(|error| CliError::io(Path::new("<stderr>"), error))?;
    drop(stderr);
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|error| CliError::io(Path::new("<stdin>"), error))?;
    Ok(answer)
}
