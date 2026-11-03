//! The immutable human-only merge rule. Orq may push feature branches and open
//! reviews, but never merges, approves, automates a merge or updates a protected
//! branch. These checks are pure so every launcher can ask them before running
//! anything, and `tests/no_merge.rs` scans the code base for the same texts.

use std::path::Path;

const PROVIDER_PROGRAMS: [&str; 2] = ["gh", "glab"];
const FORBIDDEN_SUBCOMMANDS: [&str; 2] = ["merge", "approve"];
const FORBIDDEN_FLAGS: [&str; 6] =
    ["--approve", "--auto", "--admin", "--enable-auto-merge", "--auto-merge", "--when-pipeline-succeeds"];
const FORBIDDEN_API_SUFFIXES: [&str; 3] = ["/merge", "/approve", "/auto-merge"];
const FORBIDDEN_GRAPHQL: [&str; 4] = ["mergepullrequest", "enablepullrequestautomerge", "approvepullrequest", "automerge"];
/// Flags of the provider CLIs whose next argument is a value, not a subcommand.
const VALUE_FLAGS: [&str; 8] = ["--repo", "-R", "--hostname", "-H", "--method", "-X", "--header", "-F"];

fn program_name(program: &str) -> &str {
    Path::new(program).file_name().and_then(|name| name.to_str()).unwrap_or(program)
}

/// Why a call to a Git provider CLI is forbidden, or `None` when it is allowed.
pub fn forbidden_provider_call(program: &str, args: &[String]) -> Option<&'static str> {
    if !PROVIDER_PROGRAMS.contains(&program_name(program)) {
        return None;
    }
    let mut positionals = Vec::new();
    let mut short_flags = Vec::new();
    let mut skip_value = false;
    for arg in args {
        if std::mem::take(&mut skip_value) {
            continue;
        }
        // `--flag=value` is the same flag as `--flag`.
        let name = arg.split('=').next().unwrap_or(arg);
        if FORBIDDEN_FLAGS.contains(&name) {
            return Some("a provider flag that approves or automates a merge");
        }
        if arg.starts_with("--method=") || arg.starts_with("--repo=") {
            continue;
        }
        if arg.starts_with('-') {
            if !arg.starts_with("--") {
                short_flags.push(arg.as_str());
            }
            skip_value = VALUE_FLAGS.contains(&arg.as_str());
            continue;
        }
        positionals.push(arg.as_str());
    }
    if positionals.get(1).is_some_and(|sub| FORBIDDEN_SUBCOMMANDS.contains(sub)) && matches!(positionals[0], "pr" | "mr") {
        return Some("a provider subcommand that merges or approves");
    }
    // `-a` is the short form of `gh pr review --approve`, alone or clustered (`-ab`).
    if positionals.starts_with(&["pr", "review"]) && short_flags.iter().any(|flag| flag.contains('a')) {
        return Some("a provider flag that approves or automates a merge");
    }
    if positionals.first() == Some(&"api") {
        for arg in &positionals[1..] {
            let path = arg.split('?').next().unwrap_or(arg).trim_end_matches('/');
            if FORBIDDEN_API_SUFFIXES.iter().any(|suffix| path.ends_with(suffix)) {
                return Some("a provider API endpoint that merges or approves");
            }
        }
    }
    // Only a GraphQL mutation can merge; titles, bodies and queries may mention the words freely.
    if positionals.starts_with(&["api", "graphql"]) {
        let joined = args.join(" ").to_lowercase();
        if joined.contains("mutation") && FORBIDDEN_GRAPHQL.iter().any(|needle| joined.contains(needle)) {
            return Some("a provider GraphQL mutation that merges or approves");
        }
    }
    None
}

/// Why a push (the arguments after `git`) is forbidden, or `None`. A push may
/// only update feature branches: never a protected target branch, and never
/// every ref at once.
pub fn forbidden_git_push(args: &[String], protected: &[String]) -> Option<&'static str> {
    if args.first().map(String::as_str) != Some("push") {
        return None;
    }
    if args.iter().any(|arg| arg == "--mirror" || arg == "--all") {
        return Some("a push of every ref");
    }
    let mut positionals = args[1..].iter().filter(|arg| !arg.starts_with('-'));
    let _remote = positionals.next();
    let mut explicit = 0;
    for refspec in positionals {
        let refspec = refspec.trim_start_matches('+');
        // The destination of these depends on configuration or expansion, so it cannot be checked.
        if refspec == "HEAD" || refspec.contains('*') || refspec.ends_with(':') {
            return Some("a push whose destination branches cannot be determined");
        }
        explicit += 1;
        let destination = refspec.rsplit(':').next().unwrap_or(refspec);
        let branch = destination.strip_prefix("refs/heads/").unwrap_or(destination);
        if protected.iter().any(|name| name == branch) {
            return Some("a push to a protected target branch");
        }
    }
    if explicit == 0 {
        return Some("a push without an explicit branch to update");
    }
    None
}
