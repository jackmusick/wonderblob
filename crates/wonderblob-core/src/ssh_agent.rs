//! Resolve the SSH agent socket the way OpenSSH does — honoring `IdentityAgent`
//! from `~/.ssh/config` — so agent auth finds the user's *real* agent (e.g.
//! 1Password at `~/.1password/agent.sock`) instead of only `$SSH_AUTH_SOCK`.
//!
//! russh never reads ssh_config, so without this an unset — or, worse, a *set
//! but wrong* — `$SSH_AUTH_SOCK` breaks agent auth even when `ssh` itself works.
//! On many desktops `$SSH_AUTH_SOCK` points at an empty gnome/systemd agent
//! while the user's keys live in 1Password, selected via `IdentityAgent`.
//!
//! Precedence matches OpenSSH: ssh_config's `IdentityAgent` wins over the
//! environment. Tests/automation can point at a specific config file via the
//! `WONDERBLOB_SSH_CONFIG` env var (the moral equivalent of `ssh -F`).
//!
//! Not (yet) supported: `Include`, `Match`, and `%`-token expansion. The common
//! cases — a global/`Host *` `IdentityAgent`, `~` and `${VAR}` expansion — are.

use std::path::{Path, PathBuf};

/// The agent socket to use for `host`, resolved from ssh_config + environment.
/// `None` means "no `IdentityAgent` applies — fall back to `$SSH_AUTH_SOCK`"
/// (russh's `connect_env`).
pub fn resolve_agent_socket(host: &str) -> Option<PathBuf> {
    // HOME on Unix; USERPROFILE on Windows (where HOME is usually unset).
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    let config_path = std::env::var_os("WONDERBLOB_SSH_CONFIG")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".ssh/config")))?;
    let config = std::fs::read_to_string(&config_path).ok()?;
    let raw = identity_agent_for_host(&config, host)?;
    let home = home?;
    expand_agent_value(&raw, &home, |k| std::env::var(k).ok())
}

/// First `IdentityAgent` whose `Host` block matches `host` — OpenSSH applies the
/// first obtained value, so the first match wins. Directives before any `Host`
/// line are global. Returns the raw, unexpanded value.
fn identity_agent_for_host(config: &str, host: &str) -> Option<String> {
    let mut in_match = true; // pre-`Host` directives apply to every host
    for line in config.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = split_kv(line) else {
            continue;
        };
        match key.as_str() {
            "host" => in_match = host_matches(&value, host),
            // We can't evaluate Match blocks (they depend on runtime state); be
            // conservative and treat them as non-matching rather than risk
            // applying an IdentityAgent the user scoped to a Match condition.
            "match" => in_match = false,
            "identityagent" if in_match => return Some(value),
            _ => {}
        }
    }
    None
}

/// Split an ssh_config line into a lowercased key and its value. The separator
/// is whitespace or `=`; a surrounding pair of double quotes is stripped.
fn split_kv(line: &str) -> Option<(String, String)> {
    let sep = line.find(|c: char| c == '=' || c.is_whitespace())?;
    let key = line[..sep].to_ascii_lowercase();
    let value = line[sep..]
        .trim_start_matches(|c: char| c == '=' || c.is_whitespace())
        .trim();
    let value = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value);
    if value.is_empty() {
        return None;
    }
    Some((key, value.to_string()))
}

/// OpenSSH `Host` matching: space-separated patterns with `*`/`?` wildcards. A
/// matching negated pattern (`!pat`) disqualifies the whole line; otherwise any
/// positive match wins.
fn host_matches(patterns: &str, host: &str) -> bool {
    let mut matched = false;
    for pat in patterns.split_whitespace() {
        let (neg, pat) = match pat.strip_prefix('!') {
            Some(p) => (true, p),
            None => (false, pat),
        };
        if pattern_match(pat, host) {
            if neg {
                return false;
            }
            matched = true;
        }
    }
    matched
}

/// Classic shell-style wildcard match over ASCII: `*` = any run, `?` = one char.
fn pattern_match(pat: &str, s: &str) -> bool {
    fn helper(p: &[u8], s: &[u8]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some(b'*') => helper(&p[1..], s) || (!s.is_empty() && helper(p, &s[1..])),
            Some(b'?') => !s.is_empty() && helper(&p[1..], &s[1..]),
            Some(&c) => !s.is_empty() && s[0] == c && helper(&p[1..], &s[1..]),
        }
    }
    helper(pat.as_bytes(), s.as_bytes())
}

/// Expand an `IdentityAgent` value to a socket path. `${VAR}` and a leading `~`
/// are expanded. The special values `SSH_AUTH_SOCK` and `none` mean "use the
/// environment" — returned as `None` so the caller falls back to `connect_env`.
fn expand_agent_value(
    raw: &str,
    home: &Path,
    getenv: impl Fn(&str) -> Option<String>,
) -> Option<PathBuf> {
    let v = raw.trim();
    if v.is_empty() || v == "SSH_AUTH_SOCK" || v.eq_ignore_ascii_case("none") {
        return None;
    }
    let expanded = expand_env(v, &getenv)?;
    let path = if let Some(rest) = expanded.strip_prefix("~/") {
        home.join(rest)
    } else if expanded == "~" {
        home.to_path_buf()
    } else {
        PathBuf::from(expanded)
    };
    Some(path)
}

/// Substitute `${VAR}` occurrences from `getenv`. Returns `None` if a referenced
/// variable is unset (an unresolved path is worse than falling back to env).
fn expand_env(s: &str, getenv: &impl Fn(&str) -> Option<String>) -> Option<String> {
    let mut out = String::new();
    let mut rest = s;
    while let Some(idx) = rest.find("${") {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + 2..];
        let end = after.find('}')?;
        out.push_str(&getenv(&after[..end])?);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_host_star_applies_to_any_host() {
        let cfg = "Host *\n\tIdentityAgent ~/.1password/agent.sock\n";
        assert_eq!(
            identity_agent_for_host(cfg, "anything.example.com"),
            Some("~/.1password/agent.sock".into())
        );
    }

    #[test]
    fn pre_host_directive_is_global() {
        let cfg = "IdentityAgent /global.sock\nHost specific\n  User x\n";
        assert_eq!(
            identity_agent_for_host(cfg, "whatever"),
            Some("/global.sock".into())
        );
    }

    #[test]
    fn first_matching_block_wins() {
        let cfg = "Host dev\n  IdentityAgent /a.sock\nHost *\n  IdentityAgent /b.sock\n";
        assert_eq!(identity_agent_for_host(cfg, "dev"), Some("/a.sock".into()));
        assert_eq!(identity_agent_for_host(cfg, "prod"), Some("/b.sock".into()));
    }

    #[test]
    fn no_identity_agent_returns_none() {
        let cfg = "Host *\n  User jack\n  ForwardAgent yes\n";
        assert_eq!(identity_agent_for_host(cfg, "x"), None);
    }

    #[test]
    fn negated_pattern_excludes_host() {
        let cfg = "Host * !secret\n  IdentityAgent /x.sock\n";
        assert_eq!(identity_agent_for_host(cfg, "secret"), None);
        assert_eq!(
            identity_agent_for_host(cfg, "other"),
            Some("/x.sock".into())
        );
    }

    #[test]
    fn match_block_is_treated_as_non_matching() {
        let cfg = "Match host bastion\n  IdentityAgent /m.sock\n";
        assert_eq!(identity_agent_for_host(cfg, "bastion"), None);
    }

    #[test]
    fn equals_separator_and_quotes() {
        let cfg = "Host=*\n  IdentityAgent=\"/q.sock\"\n";
        assert_eq!(identity_agent_for_host(cfg, "h"), Some("/q.sock".into()));
    }

    #[test]
    fn comments_and_blank_lines_ignored() {
        let cfg = "# a comment\n\nHost *\n\n  # inline-ish\n  IdentityAgent /c.sock\n";
        assert_eq!(identity_agent_for_host(cfg, "h"), Some("/c.sock".into()));
    }

    #[test]
    fn wildcard_question_mark() {
        let cfg = "Host db?\n  IdentityAgent /w.sock\n";
        assert_eq!(identity_agent_for_host(cfg, "db1"), Some("/w.sock".into()));
        assert_eq!(identity_agent_for_host(cfg, "db12"), None);
    }

    #[test]
    fn expand_tilde() {
        let home = Path::new("/home/jack");
        assert_eq!(
            expand_agent_value("~/.1password/agent.sock", home, |_| None),
            Some(PathBuf::from("/home/jack/.1password/agent.sock"))
        );
    }

    #[test]
    fn special_tokens_fall_back_to_env() {
        let home = Path::new("/home/jack");
        assert_eq!(expand_agent_value("SSH_AUTH_SOCK", home, |_| None), None);
        assert_eq!(expand_agent_value("none", home, |_| None), None);
        assert_eq!(expand_agent_value("None", home, |_| None), None);
    }

    #[test]
    fn expand_env_var() {
        let home = Path::new("/home/jack");
        let getenv = |k: &str| (k == "XDG_RUNTIME_DIR").then(|| "/run/user/1000".to_string());
        assert_eq!(
            expand_agent_value("${XDG_RUNTIME_DIR}/keyring/ssh", home, getenv),
            Some(PathBuf::from("/run/user/1000/keyring/ssh"))
        );
    }

    #[test]
    fn missing_env_var_falls_back_to_env() {
        let home = Path::new("/home/jack");
        assert_eq!(expand_agent_value("${NOPE}/x", home, |_| None), None);
    }

    #[test]
    fn absolute_path_passthrough() {
        let home = Path::new("/home/jack");
        assert_eq!(
            expand_agent_value("/run/user/1000/gcr/ssh", home, |_| None),
            Some(PathBuf::from("/run/user/1000/gcr/ssh"))
        );
    }
}
