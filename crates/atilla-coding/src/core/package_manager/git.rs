//! Git install-side machines: the `ensureGitRef` fetch/reset/clean/reinstall
//! reconcile and the fresh-clone flow. Mirrors pi's `installGit` / `ensureGitRef`.

use atilla_ai::seams::subprocess::{CommandOutput, CommandRequest};
use serde_json::Value;

use super::config::{
    git_capture, git_dependency_install_args, git_run, strings, PackageManagerConfig,
};
use crate::core::command_flow::{CommandFlowMachine, CommandStep};

// ---------------------------------------------------------------------------
// git reconcile: ensureGitRef (fetch / rev-parse / reset / clean / install)
// ---------------------------------------------------------------------------

/// pi's `ensureGitRef(targetDir, fetchArgs, ref)`: fetch the ref, compare local
/// HEAD to `<ref>^{commit}`, and — only when they differ — `reset --hard`,
/// `clean -fdx`, and reinstall git deps (when a package.json is present).
#[derive(Debug, Clone)]
pub struct GitEnsureRefMachine {
    cfg: PackageManagerConfig,
    target_dir: String,
    fetch_args: Vec<String>,
    commit_ref: String,
    has_package_json: bool,
    phase: EnsurePhase,
    local_head: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnsurePhase {
    Start,
    AwaitFetch,
    AwaitLocalHead,
    AwaitTargetHead,
    AwaitReset,
    AwaitClean,
    AwaitInstall,
    Done,
}

impl GitEnsureRefMachine {
    /// Plan a reconcile of `target_dir` to `ref_`, fetching via `fetch_args`.
    ///
    /// `has_package_json` is the host's `existsSync(join(targetDir,
    /// "package.json"))` after the fetch.
    pub fn new(
        cfg: &PackageManagerConfig,
        target_dir: impl Into<String>,
        fetch_args: Vec<String>,
        ref_: &str,
        has_package_json: bool,
    ) -> Self {
        Self {
            cfg: cfg.clone(),
            target_dir: target_dir.into(),
            fetch_args,
            commit_ref: format!("{ref_}^{{commit}}"),
            has_package_json,
            phase: EnsurePhase::Start,
            local_head: String::new(),
        }
    }

    fn install_request(&self) -> CommandRequest {
        let sub_args = git_dependency_install_args(self.cfg.npm_configured());
        self.cfg
            .npm_command_request(&sub_args, Some(&self.target_dir))
    }
}

impl CommandFlowMachine for GitEnsureRefMachine {
    fn start(&mut self) -> CommandStep {
        self.phase = EnsurePhase::AwaitFetch;
        CommandStep::Run {
            request: git_run(self.fetch_args.clone(), &self.target_dir),
        }
    }

    fn advance(&mut self, output: CommandOutput) -> CommandStep {
        match self.phase {
            EnsurePhase::AwaitFetch => {
                self.phase = EnsurePhase::AwaitLocalHead;
                CommandStep::Run {
                    request: git_capture(strings(&["rev-parse", "HEAD"]), &self.target_dir),
                }
            }
            EnsurePhase::AwaitLocalHead => {
                self.local_head = output.stdout.trim().to_string();
                self.phase = EnsurePhase::AwaitTargetHead;
                CommandStep::Run {
                    request: git_capture(
                        vec!["rev-parse".to_string(), self.commit_ref.clone()],
                        &self.target_dir,
                    ),
                }
            }
            EnsurePhase::AwaitTargetHead => {
                let target_head = output.stdout.trim();
                if self.local_head == target_head {
                    self.phase = EnsurePhase::Done;
                    return CommandStep::Done {
                        result: Value::Null,
                    };
                }
                self.phase = EnsurePhase::AwaitReset;
                CommandStep::Run {
                    request: git_run(
                        vec![
                            "reset".to_string(),
                            "--hard".to_string(),
                            self.commit_ref.clone(),
                        ],
                        &self.target_dir,
                    ),
                }
            }
            EnsurePhase::AwaitReset => {
                self.phase = EnsurePhase::AwaitClean;
                CommandStep::Run {
                    request: git_run(strings(&["clean", "-fdx"]), &self.target_dir),
                }
            }
            EnsurePhase::AwaitClean => {
                if self.has_package_json {
                    self.phase = EnsurePhase::AwaitInstall;
                    CommandStep::Run {
                        request: self.install_request(),
                    }
                } else {
                    self.phase = EnsurePhase::Done;
                    CommandStep::Done {
                        result: Value::Null,
                    }
                }
            }
            EnsurePhase::AwaitInstall | EnsurePhase::Start | EnsurePhase::Done => {
                self.phase = EnsurePhase::Done;
                CommandStep::Done {
                    result: Value::Null,
                }
            }
        }
    }
}

/// pi's fresh-clone `installGit`: `git clone <repo> <targetDir>`, an optional
/// `git checkout <ref>`, then a git-dependency install (when a package.json is
/// present after clone).
#[derive(Debug, Clone)]
pub struct GitCloneMachine {
    cfg: PackageManagerConfig,
    repo: String,
    target_dir: String,
    ref_: Option<String>,
    has_package_json: bool,
    phase: ClonePhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClonePhase {
    Start,
    AwaitClone,
    AwaitCheckout,
    AwaitInstall,
    Done,
}

impl GitCloneMachine {
    /// Plan a fresh clone of `repo` into `target_dir`.
    ///
    /// `has_package_json` is the host's `existsSync(join(targetDir,
    /// "package.json"))` after the clone/checkout.
    pub fn new(
        cfg: &PackageManagerConfig,
        repo: impl Into<String>,
        target_dir: impl Into<String>,
        ref_: Option<String>,
        has_package_json: bool,
    ) -> Self {
        Self {
            cfg: cfg.clone(),
            repo: repo.into(),
            target_dir: target_dir.into(),
            ref_,
            has_package_json,
            phase: ClonePhase::Start,
        }
    }

    fn install_or_done(&mut self) -> CommandStep {
        if self.has_package_json {
            self.phase = ClonePhase::AwaitInstall;
            let sub_args = git_dependency_install_args(self.cfg.npm_configured());
            CommandStep::Run {
                request: self
                    .cfg
                    .npm_command_request(&sub_args, Some(&self.target_dir)),
            }
        } else {
            self.phase = ClonePhase::Done;
            CommandStep::Done {
                result: Value::Null,
            }
        }
    }
}

impl CommandFlowMachine for GitCloneMachine {
    fn start(&mut self) -> CommandStep {
        self.phase = ClonePhase::AwaitClone;
        CommandStep::Run {
            request: CommandRequest::new(
                "git",
                vec![
                    "clone".to_string(),
                    self.repo.clone(),
                    self.target_dir.clone(),
                ],
            ),
        }
    }

    fn advance(&mut self, _output: CommandOutput) -> CommandStep {
        match self.phase {
            ClonePhase::AwaitClone => match self.ref_.clone() {
                Some(ref_) => {
                    self.phase = ClonePhase::AwaitCheckout;
                    CommandStep::Run {
                        request: git_run(vec!["checkout".to_string(), ref_], &self.target_dir),
                    }
                }
                None => self.install_or_done(),
            },
            ClonePhase::AwaitCheckout => self.install_or_done(),
            ClonePhase::AwaitInstall | ClonePhase::Start | ClonePhase::Done => {
                self.phase = ClonePhase::Done;
                CommandStep::Done {
                    result: Value::Null,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::config::{join_path, NETWORK_TIMEOUT_MS};
    use super::*;
    use crate::core::package_manager::test_support::{drive, ok, s};

    /// Drive a fresh git clone (no ref, package.json present) for `cfg`, returning
    /// the checkout dir and the planned requests. Shared by the git-clone argv
    /// tests so the setup boilerplate is not duplicated.
    fn drive_git_clone_fresh(cfg: &PackageManagerConfig) -> (String, Vec<CommandRequest>) {
        let target = join_path("/tmp/proj/agent", &["git", "github.com", "user", "repo"]);
        let mut machine = GitCloneMachine::new(cfg, "github.com/user/repo", &target, None, true);
        let (requests, _) = drive(&mut machine, vec![ok(""), ok("")]);
        (target, requests)
    }

    // --- git fresh-clone deps install argv (mirrors "should install git package
    // dependencies with --omit=dev") ---
    #[test]
    fn git_clone_then_omit_dev_install() {
        let cfg = PackageManagerConfig::new("/tmp/proj", "/tmp/proj/agent", None);
        let (target, requests) = drive_git_clone_fresh(&cfg);
        assert_eq!(
            requests[0],
            CommandRequest::new("git", s(&["clone", "github.com/user/repo", &target])),
        );
        assert_eq!(
            requests[1],
            CommandRequest::new("npm", s(&["install", "--omit=dev"])).with_cwd(&target),
        );
    }

    // --- plain install when npm command configured (mirrors "should use plain
    // install for git package dependencies when npmCommand is configured") ---
    #[test]
    fn git_clone_plain_install_when_configured() {
        let cfg = PackageManagerConfig::new("/tmp/proj", "/tmp/proj/agent", Some(s(&["pnpm"])));
        let (target, requests) = drive_git_clone_fresh(&cfg);
        assert_eq!(
            requests[1],
            CommandRequest::new("pnpm", s(&["install"])).with_cwd(&target),
        );
    }

    // --- ensureGitRef reconcile to pinned ref (mirrors "should reconcile an
    // existing git checkout to a pinned ref during install") ---
    #[test]
    fn git_ensure_ref_reconciles_pinned_ref() {
        let cfg = PackageManagerConfig::new("/tmp/proj", "/tmp/proj/agent", None);
        let target = join_path("/tmp/proj/agent", &["git", "github.com", "user", "repo"]);
        let mut machine = GitEnsureRefMachine::new(
            &cfg,
            &target,
            s(&["fetch", "origin", "v2"]),
            "FETCH_HEAD",
            true,
        );
        // fetch, rev-parse HEAD -> old, rev-parse FETCH_HEAD^{commit} -> new,
        // reset, clean, npm install.
        let (requests, _) = drive(
            &mut machine,
            vec![
                ok(""),
                ok("old-head"),
                ok("new-head"),
                ok(""),
                ok(""),
                ok(""),
            ],
        );
        assert_eq!(
            requests[0],
            CommandRequest::new("git", s(&["fetch", "origin", "v2"])).with_cwd(&target),
        );
        assert_eq!(
            requests[1],
            CommandRequest::new("git", s(&["rev-parse", "HEAD"]))
                .with_cwd(&target)
                .with_timeout(NETWORK_TIMEOUT_MS),
        );
        assert_eq!(
            requests[2],
            CommandRequest::new("git", s(&["rev-parse", "FETCH_HEAD^{commit}"]))
                .with_cwd(&target)
                .with_timeout(NETWORK_TIMEOUT_MS),
        );
        assert_eq!(
            requests[3],
            CommandRequest::new("git", s(&["reset", "--hard", "FETCH_HEAD^{commit}"]))
                .with_cwd(&target),
        );
        assert_eq!(
            requests[4],
            CommandRequest::new("git", s(&["clean", "-fdx"])).with_cwd(&target),
        );
        assert_eq!(
            requests[5],
            CommandRequest::new("npm", s(&["install", "--omit=dev"])).with_cwd(&target),
        );
    }

    // --- ensureGitRef early-exit when heads match ---
    #[test]
    fn git_ensure_ref_skips_when_heads_match() {
        let cfg = PackageManagerConfig::new("/tmp/proj", "/tmp/proj/agent", None);
        let target = "/tmp/checkout";
        let mut machine = GitEnsureRefMachine::new(
            &cfg,
            target,
            s(&["fetch", "origin", "v2"]),
            "FETCH_HEAD",
            true,
        );
        let (requests, _) = drive(&mut machine, vec![ok(""), ok("same"), ok("same")]);
        // fetch + two rev-parse captures only; no reset/clean/install.
        assert_eq!(requests.len(), 3);
    }

    // --- ensureGitRef via update target, no package.json (mirrors "should
    // reconcile an existing git checkout to its update target when installing
    // without a ref") ---
    #[test]
    fn git_ensure_ref_update_target_without_package_json() {
        let cfg = PackageManagerConfig::new("/tmp/proj", "/tmp/proj/agent", None);
        let target = join_path("/tmp/proj/agent", &["git", "github.com", "user", "repo"]);
        let fetch_args = s(&[
            "fetch",
            "--prune",
            "--no-tags",
            "origin",
            "+refs/heads/main:refs/remotes/origin/main",
        ]);
        let mut machine =
            GitEnsureRefMachine::new(&cfg, &target, fetch_args.clone(), "origin/HEAD", false);
        let (requests, _) = drive(
            &mut machine,
            vec![ok(""), ok("old-head"), ok("new-head"), ok(""), ok("")],
        );
        assert_eq!(
            requests[0],
            CommandRequest::new("git", fetch_args).with_cwd(&target)
        );
        assert_eq!(
            requests[3],
            CommandRequest::new("git", s(&["reset", "--hard", "origin/HEAD^{commit}"]))
                .with_cwd(&target),
        );
        assert_eq!(
            requests[4],
            CommandRequest::new("git", s(&["clean", "-fdx"])).with_cwd(&target),
        );
        // No package.json -> no npm install.
        assert_eq!(requests.len(), 5);
    }

    // --- update git deps through wrapped pnpm (mirrors "should use plain
    // install through npmCommand argv when updating git package dependencies") ---
    #[test]
    fn git_ensure_ref_wrapped_pnpm_install() {
        let cfg = PackageManagerConfig::new(
            "/tmp/proj",
            "/tmp/proj/agent",
            Some(s(&["mise", "exec", "node@20", "--", "pnpm"])),
        );
        let target = join_path("/tmp/proj", &[".pi", "git", "github.com", "user", "repo"]);
        let mut machine = GitEnsureRefMachine::new(
            &cfg,
            &target,
            s(&["fetch", "origin", "main"]),
            "@{upstream}",
            true,
        );
        let (requests, _) = drive(
            &mut machine,
            vec![
                ok(""),
                ok("local-head"),
                ok("remote-head"),
                ok(""),
                ok(""),
                ok(""),
            ],
        );
        assert_eq!(
            requests[5],
            CommandRequest::new("mise", s(&["exec", "node@20", "--", "pnpm", "install"]))
                .with_cwd(&target),
        );
    }
}
