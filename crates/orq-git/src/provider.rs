//! Which Git hosting provider a remote belongs to, deciding whether `gh`
//! (GitHub pull requests) or `glab` (GitLab merge requests) drives a run.
//!
//! Pure string handling, no I/O. Unlike the Bash orchestrator, which treats
//! every host that is not `github.com` as GitLab, an unrecognised host is an
//! error: a wrong guess would send a push or a review request to the wrong CLI.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    GitHub,
    GitLab,
}

impl Provider {
    /// The command-line client for this provider.
    pub fn cli(self) -> &'static str {
        match self {
            Provider::GitHub => "gh",
            Provider::GitLab => "glab",
        }
    }
}

/// Extra self-hosted hostnames (no scheme or port), beyond `github.com` and
/// `gitlab.com`, matched case-insensitively.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderHosts {
    pub github: Vec<String>,
    pub gitlab: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// The explicit forge setting is neither GitHub nor GitLab.
    InvalidForge(String),
    /// The remote URL has no hostname (a local path, `file://`, or malformed).
    NoHost { url: String },
    /// A hostname neither built in nor configured.
    UnknownHost { host: String },
    /// A hostname configured for both providers.
    AmbiguousHost { host: String },
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderError::InvalidForge(forge) => write!(f, "forge must be gh or glab, got {forge:?}"),
            ProviderError::NoHost { url } => write!(f, "remote {url:?} has no host to detect a provider from"),
            ProviderError::UnknownHost { host } => write!(
                f,
                "unknown git host {host:?}: set the forge explicitly or list the host in the settings"
            ),
            ProviderError::AmbiguousHost { host } => {
                write!(f, "git host {host:?} is configured for both GitHub and GitLab")
            }
        }
    }
}

impl std::error::Error for ProviderError {}

/// `gh`/`github` or `glab`/`gitlab`, ignoring case and surrounding space.
pub fn parse_forge(text: &str) -> Result<Provider, ProviderError> {
    let text = text.trim();
    match text.to_ascii_lowercase().as_str() {
        "gh" | "github" => Ok(Provider::GitHub),
        "glab" | "gitlab" => Ok(Provider::GitLab),
        _ => Err(ProviderError::InvalidForge(text.to_string())),
    }
}

/// The lowercase hostname of a remote URL: `http(s)`, `ssh`, `git` and
/// scp-like (`user@host:path`) forms. Userinfo, port and path are dropped, so
/// `https://github.com@evil.example/x` is `evil.example`.
pub fn remote_host(url: &str) -> Option<String> {
    let url = url.trim();
    let authority = match url.split_once("://") {
        Some((scheme, rest)) => {
            let scheme = scheme.to_ascii_lowercase();
            if !matches!(scheme.as_str(), "http" | "https" | "ssh" | "git" | "git+ssh" | "ssh+git") {
                return None;
            }
            // The authority ends at the first path, query or fragment delimiter.
            rest.split(['/', '?', '#', '\\']).next().unwrap_or("")
        }
        None => {
            // scp-like `[user@]host:path`: the colon must precede any slash.
            let colon = url.find(':')?;
            let before = &url[..colon];
            if before.contains('/') {
                return None;
            }
            before
        }
    };
    let host_port = authority.rsplit('@').next().unwrap_or("");
    let host = if let Some(bracketed) = host_port.strip_prefix('[') {
        let end = bracketed.find(']')?;
        format!("[{}]", &bracketed[..end])
    } else {
        match host_port.rsplit_once(':') {
            Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host.to_string(),
            Some(_) => return None,
            None => host_port.to_string(),
        }
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || host.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    Some(host)
}

fn normalize(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// The provider for `remote_url`. A non-empty `forge_override` (`gh` or `glab`)
/// wins over the URL, whatever it is; otherwise the URL's host must be
/// `github.com`, `gitlab.com` or listed in `hosts`.
pub fn detect_provider(
    remote_url: &str,
    hosts: &ProviderHosts,
    forge_override: Option<&str>,
) -> Result<Provider, ProviderError> {
    if let Some(forge) = forge_override.map(str::trim).filter(|forge| !forge.is_empty()) {
        return parse_forge(forge);
    }
    let host = remote_host(remote_url).ok_or_else(|| ProviderError::NoHost {
        url: remote_url.to_string(),
    })?;
    let listed = |configured: &[String], builtin: &str| host == builtin || configured.iter().any(|h| normalize(h) == host);
    match (listed(&hosts.github, "github.com"), listed(&hosts.gitlab, "gitlab.com")) {
        (true, false) => Ok(Provider::GitHub),
        (false, true) => Ok(Provider::GitLab),
        (true, true) => Err(ProviderError::AmbiguousHost { host }),
        (false, false) => Err(ProviderError::UnknownHost { host }),
    }
}
