//! Git update-resolution machines: the local update-target resolution
//! (`getLocalGitUpdateTarget`), remote-head lookup (`getRemoteGitHead`), and
//! the available-update check (`gitHasAvailableUpdate`).

use atilla_ai::seams::subprocess::CommandOutput;
use serde::Serialize;
use serde_json::{json, Value};

use super::config::{git_capture, git_remote, git_run, strings};
use crate::core::command_flow::{CommandFlowMachine, CommandStep};

// ---------------------------------------------------------------------------
// git upstream / update-target resolution
// ---------------------------------------------------------------------------

/// The resolved local git update target, mirroring pi's
/// `getLocalGitUpdateTarget` return `{ ref, head, fetchArgs }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitUpdateTarget {
    /// The ref to reconcile to (`@{upstream}` or `origin/HEAD`). `ref` is a Rust
    /// keyword, so the field is `ref_` but serializes to exactly `ref`.
    #[serde(rename = "ref")]
    pub ref_: String,
    /// The resolved head commit of that ref.
    pub head: String,
    /// The `git fetch` argv to fetch it (serializes to `fetchArgs`).
    pub fetch_args: Vec<String>,
}

fn upstream_fetch_args(branch: &str) -> Vec<String> {
    vec![
        "fetch".to_string(),
        "--prune".to_string(),
        "--no-tags".to_string(),
        "origin".to_string(),
        format!("+refs/heads/{branch}:refs/remotes/origin/{branch}"),
    ]
}

/// pi's `getLocalGitUpdateTarget(installedPath)`: resolve the fetch/reset target
/// from the tracking branch, with the `remote set-head` / `symbolic-ref`
/// fallback chain when there is no usable `@{upstream}`.
#[derive(Debug, Clone)]
pub struct GitLocalUpdateTargetMachine {
    installed_path: String,
    phase: TargetPhase,
    head: String,
    pending_fetch_args: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetPhase {
    Start,
    AwaitUpstream,
    AwaitUpstreamHead,
    AwaitSetHead,
    AwaitOriginHead,
    AwaitSymbolicRef,
    Done,
}

impl GitLocalUpdateTargetMachine {
    /// Plan the update-target resolution for `installed_path`.
    pub fn new(installed_path: impl Into<String>) -> Self {
        Self {
            installed_path: installed_path.into(),
            phase: TargetPhase::Start,
            head: String::new(),
            pending_fetch_args: Vec::new(),
        }
    }

    fn enter_fallback(&mut self) -> CommandStep {
        self.phase = TargetPhase::AwaitSetHead;
        CommandStep::Run {
            request: git_run(
                strings(&["remote", "set-head", "origin", "-a"]),
                &self.installed_path,
            ),
        }
    }

    fn done(&self, target: GitUpdateTarget) -> CommandStep {
        CommandStep::Done {
            result: serde_json::to_value(&target).expect("GitUpdateTarget serializes"),
        }
    }
}

impl CommandFlowMachine for GitLocalUpdateTargetMachine {
    fn start(&mut self) -> CommandStep {
        self.phase = TargetPhase::AwaitUpstream;
        CommandStep::Run {
            request: git_capture(
                strings(&["rev-parse", "--abbrev-ref", "@{upstream}"]),
                &self.installed_path,
            ),
        }
    }

    fn advance(&mut self, output: CommandOutput) -> CommandStep {
        match self.phase {
            TargetPhase::AwaitUpstream => {
                let trimmed = output.stdout.trim();
                if !output.success() {
                    return self.enter_fallback();
                }
                let Some(branch) = trimmed.strip_prefix("origin/") else {
                    return self.enter_fallback();
                };
                if branch.is_empty() {
                    return self.enter_fallback();
                }
                // Stash the fetch args (built from the validated branch) for the
                // Done step after we resolve the upstream head.
                self.pending_fetch_args = upstream_fetch_args(branch);
                self.phase = TargetPhase::AwaitUpstreamHead;
                CommandStep::Run {
                    request: git_capture(
                        strings(&["rev-parse", "@{upstream}"]),
                        &self.installed_path,
                    ),
                }
            }
            TargetPhase::AwaitUpstreamHead => {
                self.phase = TargetPhase::Done;
                self.done(GitUpdateTarget {
                    ref_: "@{upstream}".to_string(),
                    head: output.stdout.trim().to_string(),
                    fetch_args: self.pending_fetch_args.clone(),
                })
            }
            TargetPhase::AwaitSetHead => {
                // set-head failure is ignored (pi's `.catch(() => {})`).
                self.phase = TargetPhase::AwaitOriginHead;
                CommandStep::Run {
                    request: git_capture(
                        strings(&["rev-parse", "origin/HEAD"]),
                        &self.installed_path,
                    ),
                }
            }
            TargetPhase::AwaitOriginHead => {
                self.head = output.stdout.trim().to_string();
                self.phase = TargetPhase::AwaitSymbolicRef;
                CommandStep::Run {
                    request: git_capture(
                        strings(&["symbolic-ref", "refs/remotes/origin/HEAD"]),
                        &self.installed_path,
                    ),
                }
            }
            TargetPhase::AwaitSymbolicRef => {
                let origin_head_ref = if output.success() {
                    output.stdout.trim()
                } else {
                    ""
                };
                let branch = origin_head_ref
                    .strip_prefix("refs/remotes/origin/")
                    .unwrap_or("");
                let fetch_args = if !branch.is_empty() {
                    upstream_fetch_args(branch)
                } else {
                    strings(&[
                        "fetch",
                        "--prune",
                        "--no-tags",
                        "origin",
                        "+HEAD:refs/remotes/origin/HEAD",
                    ])
                };
                self.phase = TargetPhase::Done;
                self.done(GitUpdateTarget {
                    ref_: "origin/HEAD".to_string(),
                    head: self.head.clone(),
                    fetch_args,
                })
            }
            TargetPhase::Start | TargetPhase::Done => self.done(GitUpdateTarget {
                ref_: "origin/HEAD".to_string(),
                head: self.head.clone(),
                fetch_args: Vec::new(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// git remote head resolution + available-update check
// ---------------------------------------------------------------------------

/// The outcome of resolving a remote git head.
///
/// pi's `getRemoteGitHead` resolves to the SHA string or throws; the
/// `#[serde(untagged)]` shape mirrors that across the napi boundary — `Head`
/// serializes to the bare SHA string and `Failed` to `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum RemoteHead {
    /// The resolved 40-char commit SHA.
    Head(String),
    /// pi threw "Failed to determine remote HEAD" (or a probe rejected).
    Failed,
}

fn first_sha(text: &str) -> Option<String> {
    let re = regex::Regex::new(r"(?m)^([0-9a-f]{40})\s+").expect("valid regex");
    re.captures(text).map(|caps| caps[1].to_string())
}

fn head_sha(text: &str) -> Option<String> {
    let re = regex::Regex::new(r"(?m)^([0-9a-f]{40})\s+HEAD$").expect("valid regex");
    re.captures(text).map(|caps| caps[1].to_string())
}

/// pi's `getRemoteGitHead(installedPath)`: probe the tracking branch
/// (`rev-parse --abbrev-ref @{upstream}`), `ls-remote origin <upstreamRef>` when
/// there is one, and fall back to `ls-remote origin HEAD`. Remote reads carry
/// `GIT_TERMINAL_PROMPT=0` and the network timeout.
#[derive(Debug, Clone)]
pub struct GitRemoteHeadMachine {
    installed_path: String,
    phase: RemoteHeadPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteHeadPhase {
    Start,
    AwaitUpstream,
    AwaitUpstreamLsRemote,
    AwaitHeadLsRemote,
    Done,
}

impl GitRemoteHeadMachine {
    /// Plan the remote-head resolution for `installed_path`.
    pub fn new(installed_path: impl Into<String>) -> Self {
        Self {
            installed_path: installed_path.into(),
            phase: RemoteHeadPhase::Start,
        }
    }

    fn ls_remote_head(&mut self) -> CommandStep {
        self.phase = RemoteHeadPhase::AwaitHeadLsRemote;
        CommandStep::Run {
            request: git_remote(
                strings(&["ls-remote", "origin", "HEAD"]),
                &self.installed_path,
            ),
        }
    }

    fn done(head: RemoteHead) -> CommandStep {
        CommandStep::Done {
            result: json!(head),
        }
    }
}

impl CommandFlowMachine for GitRemoteHeadMachine {
    fn start(&mut self) -> CommandStep {
        self.phase = RemoteHeadPhase::AwaitUpstream;
        CommandStep::Run {
            request: git_capture(
                strings(&["rev-parse", "--abbrev-ref", "@{upstream}"]),
                &self.installed_path,
            ),
        }
    }

    fn advance(&mut self, output: CommandOutput) -> CommandStep {
        match self.phase {
            RemoteHeadPhase::AwaitUpstream => {
                // getGitUpstreamRef: only origin/<branch> yields a usable ref.
                let upstream_ref = if output.success() {
                    output
                        .stdout
                        .trim()
                        .strip_prefix("origin/")
                        .filter(|branch| !branch.is_empty())
                        .map(|branch| format!("refs/heads/{branch}"))
                } else {
                    None
                };
                match upstream_ref {
                    Some(ref_) => {
                        self.phase = RemoteHeadPhase::AwaitUpstreamLsRemote;
                        CommandStep::Run {
                            request: git_remote(
                                vec!["ls-remote".to_string(), "origin".to_string(), ref_],
                                &self.installed_path,
                            ),
                        }
                    }
                    None => self.ls_remote_head(),
                }
            }
            RemoteHeadPhase::AwaitUpstreamLsRemote => {
                if !output.success() {
                    self.phase = RemoteHeadPhase::Done;
                    return Self::done(RemoteHead::Failed);
                }
                match first_sha(&output.stdout) {
                    Some(sha) => {
                        self.phase = RemoteHeadPhase::Done;
                        Self::done(RemoteHead::Head(sha))
                    }
                    None => self.ls_remote_head(),
                }
            }
            RemoteHeadPhase::AwaitHeadLsRemote => {
                self.phase = RemoteHeadPhase::Done;
                let result = if !output.success() {
                    RemoteHead::Failed
                } else {
                    match head_sha(&output.stdout) {
                        Some(sha) => RemoteHead::Head(sha),
                        None => RemoteHead::Failed,
                    }
                };
                Self::done(result)
            }
            RemoteHeadPhase::Start | RemoteHeadPhase::Done => Self::done(RemoteHead::Failed),
        }
    }
}

/// pi's `gitHasAvailableUpdate(installedPath)`: compare local `rev-parse HEAD`
/// with the resolved remote head; any probe failure yields `false`.
#[derive(Debug, Clone)]
pub struct GitHasUpdateMachine {
    installed_path: String,
    phase: HasUpdatePhase,
    local_head: String,
    remote: GitRemoteHeadMachine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HasUpdatePhase {
    Start,
    AwaitLocalHead,
    InRemote,
    Done,
}

impl GitHasUpdateMachine {
    /// Plan the available-update check for `installed_path`.
    pub fn new(installed_path: impl Into<String>) -> Self {
        let installed_path = installed_path.into();
        Self {
            remote: GitRemoteHeadMachine::new(installed_path.clone()),
            installed_path,
            phase: HasUpdatePhase::Start,
            local_head: String::new(),
        }
    }
}

impl CommandFlowMachine for GitHasUpdateMachine {
    fn start(&mut self) -> CommandStep {
        self.phase = HasUpdatePhase::AwaitLocalHead;
        CommandStep::Run {
            request: git_capture(strings(&["rev-parse", "HEAD"]), &self.installed_path),
        }
    }

    fn advance(&mut self, output: CommandOutput) -> CommandStep {
        match self.phase {
            HasUpdatePhase::AwaitLocalHead => {
                if !output.success() {
                    self.phase = HasUpdatePhase::Done;
                    return CommandStep::Done {
                        result: json!(false),
                    };
                }
                self.local_head = output.stdout.trim().to_string();
                self.phase = HasUpdatePhase::InRemote;
                let step = self.remote.start();
                self.wrap_remote(step)
            }
            HasUpdatePhase::InRemote => {
                let step = self.remote.advance(output);
                self.wrap_remote(step)
            }
            HasUpdatePhase::Start | HasUpdatePhase::Done => CommandStep::Done {
                result: json!(false),
            },
        }
    }
}

impl GitHasUpdateMachine {
    /// Forward the embedded remote-head machine's step, finishing with the
    /// local-vs-remote comparison once the remote resolves.
    fn wrap_remote(&mut self, step: CommandStep) -> CommandStep {
        match step {
            run @ CommandStep::Run { .. } => run,
            CommandStep::Done { result } => {
                self.phase = HasUpdatePhase::Done;
                CommandStep::Done {
                    result: json!(self.compare(result)),
                }
            }
        }
    }

    fn compare(&self, remote: Value) -> bool {
        // RemoteHead serializes untagged: a bare SHA string, or null on failure.
        match remote.as_str() {
            Some(head) => self.local_head != head.trim(),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::config::NETWORK_TIMEOUT_MS;
    use super::*;
    use crate::core::package_manager::test_support::{drive, fail, ok, s};
    use atilla_ai::seams::subprocess::CommandRequest;

    // --- getLocalGitUpdateTarget upstream path (argv-depends-on-output) ---
    #[test]
    fn local_update_target_upstream_branch() {
        let mut machine = GitLocalUpdateTargetMachine::new("/tmp/checkout");
        let (requests, target) = drive(&mut machine, vec![ok("origin/main"), ok("remote-head")]);
        assert_eq!(
            requests[0],
            CommandRequest::new("git", s(&["rev-parse", "--abbrev-ref", "@{upstream}"]))
                .with_cwd("/tmp/checkout")
                .with_timeout(NETWORK_TIMEOUT_MS),
        );
        assert_eq!(
            requests[1],
            CommandRequest::new("git", s(&["rev-parse", "@{upstream}"]))
                .with_cwd("/tmp/checkout")
                .with_timeout(NETWORK_TIMEOUT_MS),
        );
        assert_eq!(
            target,
            json!({
                "ref": "@{upstream}",
                "head": "remote-head",
                "fetchArgs": [
                    "fetch",
                    "--prune",
                    "--no-tags",
                    "origin",
                    "+refs/heads/main:refs/remotes/origin/main",
                ],
            })
        );
    }

    // --- getLocalGitUpdateTarget fallback chain (set-head -> rev-parse ->
    // symbolic-ref) ---
    #[test]
    fn local_update_target_fallback_chain() {
        let mut machine = GitLocalUpdateTargetMachine::new("/tmp/checkout");
        // upstream probe fails -> set-head, rev-parse origin/HEAD, symbolic-ref.
        let (requests, target) = drive(
            &mut machine,
            vec![
                fail(),
                ok(""),
                ok("head-sha"),
                ok("refs/remotes/origin/trunk"),
            ],
        );
        assert_eq!(
            requests[1],
            CommandRequest::new("git", s(&["remote", "set-head", "origin", "-a"]))
                .with_cwd("/tmp/checkout"),
        );
        assert_eq!(
            requests[2],
            CommandRequest::new("git", s(&["rev-parse", "origin/HEAD"]))
                .with_cwd("/tmp/checkout")
                .with_timeout(NETWORK_TIMEOUT_MS),
        );
        assert_eq!(
            requests[3],
            CommandRequest::new("git", s(&["symbolic-ref", "refs/remotes/origin/HEAD"]))
                .with_cwd("/tmp/checkout")
                .with_timeout(NETWORK_TIMEOUT_MS),
        );
        assert_eq!(
            target,
            json!({
                "ref": "origin/HEAD",
                "head": "head-sha",
                "fetchArgs": [
                    "fetch",
                    "--prune",
                    "--no-tags",
                    "origin",
                    "+refs/heads/trunk:refs/remotes/origin/trunk",
                ],
            })
        );
    }

    #[test]
    fn local_update_target_fallback_no_symbolic_ref() {
        let mut machine = GitLocalUpdateTargetMachine::new("/tmp/checkout");
        // upstream is non-origin -> set-head, rev-parse origin/HEAD, symbolic-ref
        // (which fails), yielding the +HEAD fallback fetch args.
        let (_requests, target) = drive(
            &mut machine,
            vec![ok("weird/remote"), ok(""), ok("head-sha"), fail()],
        );
        assert_eq!(
            target["fetchArgs"],
            json!([
                "fetch",
                "--prune",
                "--no-tags",
                "origin",
                "+HEAD:refs/remotes/origin/HEAD",
            ])
        );
        assert_eq!(target["ref"], "origin/HEAD");
    }

    // --- getRemoteGitHead upstream ls-remote chain ---
    #[test]
    fn remote_head_upstream_ls_remote() {
        let mut machine = GitRemoteHeadMachine::new("/tmp/checkout");
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let (requests, result) = drive(
            &mut machine,
            vec![ok("origin/main"), ok(&format!("{sha}\trefs/heads/main"))],
        );
        assert_eq!(
            requests[1],
            CommandRequest::new("git", s(&["ls-remote", "origin", "refs/heads/main"]))
                .with_cwd("/tmp/checkout")
                .with_env("GIT_TERMINAL_PROMPT", "0")
                .with_timeout(NETWORK_TIMEOUT_MS),
        );
        assert_eq!(result, json!(sha));
    }

    #[test]
    fn remote_head_falls_back_to_head_ls_remote() {
        let mut machine = GitRemoteHeadMachine::new("/tmp/checkout");
        let sha = "0123456789abcdef0123456789abcdef01234567";
        // No upstream -> ls-remote origin HEAD.
        let (requests, result) = drive(&mut machine, vec![fail(), ok(&format!("{sha}\tHEAD"))]);
        assert_eq!(
            requests[1],
            CommandRequest::new("git", s(&["ls-remote", "origin", "HEAD"]))
                .with_cwd("/tmp/checkout")
                .with_env("GIT_TERMINAL_PROMPT", "0")
                .with_timeout(NETWORK_TIMEOUT_MS),
        );
        assert_eq!(result, json!(sha));
    }

    // --- gitHasAvailableUpdate boolean ---
    #[test]
    fn git_has_update_true_when_heads_differ() {
        let mut machine = GitHasUpdateMachine::new("/tmp/checkout");
        let remote = "0123456789abcdef0123456789abcdef01234567";
        let (requests, has_update) = drive(
            &mut machine,
            vec![
                ok("localsha"),
                ok("origin/main"),
                ok(&format!("{remote}\trefs/heads/main")),
            ],
        );
        assert_eq!(
            requests[0],
            CommandRequest::new("git", s(&["rev-parse", "HEAD"]))
                .with_cwd("/tmp/checkout")
                .with_timeout(NETWORK_TIMEOUT_MS),
        );
        assert_eq!(has_update, json!(true));
    }

    #[test]
    fn git_has_update_false_when_local_probe_fails() {
        let mut machine = GitHasUpdateMachine::new("/tmp/checkout");
        let (requests, has_update) = drive(&mut machine, vec![fail()]);
        assert_eq!(requests.len(), 1);
        assert_eq!(has_update, json!(false));
    }
}
