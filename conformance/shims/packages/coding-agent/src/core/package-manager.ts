// straitjacket-allow-file:duplication — the command-PLANNING overrides below
// share a faithful "guard non-managed scope / delegate to pi's original via
// prototype.call / build a CommandCore and drive it" shape; each mirrors a
// distinct pi package-manager operation and is kept separate on purpose.
// Native shim for packages/coding-agent/src/core/package-manager.ts, backed by
// the atilla Rust addon (`atilla-napi`, `CommandCore`). Installed by
// conformance/codegen.mjs when the module is marked `native` in
// conformance/manifest.json: the original pi file is preserved alongside as
// `package-manager.__pi_original__.ts` and this shim takes its place, so pi's
// callers and test/package-manager.test.ts import
// `../src/core/package-manager.ts` unchanged.
//
// Scope of the native flip
// ------------------------
// pi's `DefaultPackageManager` mixes pure filesystem resolution with external
// command execution, reaching the outside world through three private runners
// (`runCommand`, `runCommandCapture`, `runCommandSync`). The 43-site
// command-mock cohort of `package-manager.test.ts` spies those runners and
// asserts the exact argv (and, where present, `cwd` / `timeoutMs` / `env`) each
// operation plans.
//
// This shim subclasses pi's original `DefaultPackageManager` and overrides the
// command-PLANNING methods so the argv now comes from the Rust command-flow
// machines (driven through the napi `CommandCore`) instead of pi's TypeScript.
// Each override drives a `CommandCore` and executes every planned
// `CommandRequest` through the INHERITED runner — so the spies still attach to
// `runCommand` / `runCommandCapture` / `runCommandSync` and observe the argv the
// Rust machine planned. Everything else (source parsing, path resolution,
// settings, `resolve`, progress, dedupe) stays pi's original, inherited
// unchanged.
//
// Flipped through Rust: npm install / uninstall / batch-install, git fresh-clone
// + dependency install, the `ensureGitRef` fetch/reset/clean/reinstall
// reconcile, the `npm root -g` global-root probe, and the `pnpm list -g`
// global-path probe. Left as pi's original (inherited): the git update-target
// resolution (`getLocalGitUpdateTarget`, `getRemoteGitHead`,
// `gitHasAvailableUpdate`), which carry pi-side offline guards and throw
// semantics the tests exercise directly and which the machines do not gate; and
// `getLatestNpmVersion` (the `npm view` version probe), because pi's
// `parseSource` expands ranges into node-semver syntax (e.g.
// `>=1.0.0 <2.0.0-0`) that the Rust machine's Cargo-style `semver::VersionReq`
// cannot parse — flipping it would silently mis-select versions. The `npm view`
// argv is still asserted; it just plans through pi's TypeScript, not Rust.

import { existsSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { CommandCore } from "atilla-napi";
import { DefaultPackageManager as PiDefaultPackageManager } from "./package-manager.__pi_original__.ts";

export * from "./package-manager.__pi_original__.ts";

// --- driver-loop plumbing ---------------------------------------------------

/** Wire shape of a planned command (matches the Rust `CommandRequest`). */
interface CommandRequest {
	program: string;
	args: string[];
	cwd: string | null;
	env: [string, string][];
	timeoutMs: number | null;
}

/** Wire shape of a run result fed back to the machine. */
interface CommandOutput {
	code: number | null;
	stdout: string;
	stderr: string;
}

type CommandStep = { type: "run"; request: CommandRequest } | { type: "done"; result: unknown };

/** The inherited private runners this shim drives commands through. */
interface Runners {
	runCommand(command: string, args: string[], options?: { cwd?: string }): Promise<void>;
	runCommandCapture(
		command: string,
		args: string[],
		options?: { cwd?: string; timeoutMs?: number; env?: Record<string, string> },
	): Promise<string>;
	runCommandSync(command: string, args: string[]): string;
}

/** pi passes `options` undefined (not `{}`) when there is no cwd. */
function runOptions(request: CommandRequest): { cwd?: string } | undefined {
	return request.cwd === null ? undefined : { cwd: request.cwd };
}

/**
 * Execute one planned request via the appropriate async runner. A request with
 * a `timeoutMs` is a capture (pi's `runCommandCapture`); otherwise it is a plain
 * void run (pi's `runCommand`). A capture rejection (non-zero exit / spawn
 * error) is converted to a failed `CommandOutput` so the machine sees the same
 * `!success` signal pi's `.catch` / try-blocks handle.
 */
async function execAsync(runners: Runners, request: CommandRequest): Promise<CommandOutput> {
	if (request.timeoutMs !== null) {
		const options: { cwd?: string; timeoutMs?: number; env?: Record<string, string> } = {
			timeoutMs: request.timeoutMs,
		};
		if (request.cwd !== null) options.cwd = request.cwd;
		if (request.env.length > 0) options.env = Object.fromEntries(request.env);
		try {
			const stdout = await runners.runCommandCapture(request.program, request.args, options);
			return { code: 0, stdout, stderr: "" };
		} catch (error) {
			return { code: 1, stdout: "", stderr: error instanceof Error ? error.message : String(error) };
		}
	}
	await runners.runCommand(request.program, request.args, runOptions(request));
	return { code: 0, stdout: "", stderr: "" };
}

/** Drive an async command flow to completion, returning the `done` result. */
async function driveAsync(core: CommandCore, runners: Runners): Promise<unknown> {
	let step = JSON.parse(core.start()) as CommandStep;
	while (step.type === "run") {
		const output = await execAsync(runners, step.request);
		step = JSON.parse(core.advance(JSON.stringify(output))) as CommandStep;
	}
	return step.result;
}

/** Drive a synchronous command flow (pi's `runCommandSync` operations). */
function driveSync(core: CommandCore, runners: Runners): unknown {
	let step = JSON.parse(core.start()) as CommandStep;
	while (step.type === "run") {
		const stdout = runners.runCommandSync(step.request.program, step.request.args);
		step = JSON.parse(core.advance(JSON.stringify({ code: 0, stdout, stderr: "" }))) as CommandStep;
	}
	return step.result;
}

/** The package-manager config the command argv depends on (pi's cwd/agentDir/npmCommand). */
function configJson(pm: unknown): { cwd: string; agentDir: string; npmCommand: string[] | null } {
	// cwd/agentDir/settingsManager are private on the base; read them off the instance.
	const self = pm as { cwd: string; agentDir: string; settingsManager: { getNpmCommand(): string[] | undefined } };
	return {
		cwd: self.cwd,
		agentDir: self.agentDir,
		npmCommand: self.settingsManager.getNpmCommand() ?? null,
	};
}

// Minimal structural views of the parsed-source shapes pi hands the overridden
// methods (only the fields the Rust ops consume).
type NpmSource = { type: "npm"; spec: string; name: string; pinned: boolean };
type GitSource = { type: "git"; repo: string; host: string; path: string; pinned: boolean; ref?: string };
type SourceScope = "user" | "project" | "temporary";

// --- native subclass --------------------------------------------------------

export class DefaultPackageManager extends PiDefaultPackageManager {
	// npm install (single spec). Temporary scope keeps pi's original (the Rust
	// InstallScope models only the managed user/project roots).
	private async installNpm(source: NpmSource, scope: SourceScope, temporary: boolean): Promise<void> {
		if (temporary || (scope !== "user" && scope !== "project")) {
			return (PiDefaultPackageManager.prototype as unknown as { installNpm: DefaultPackageManager["installNpm"] })
				.installNpm.call(this, source, scope, temporary);
		}
		const self = this as unknown as { getNpmInstallRoot(s: SourceScope, t: boolean): string; ensureNpmProject(r: string): void };
		const installRoot = self.getNpmInstallRoot(scope, false);
		self.ensureNpmProject(installRoot);
		const core = new CommandCore(
			"npmInstall",
			JSON.stringify({ config: configJson(this), specs: [source.spec], scope }),
		);
		await driveAsync(core, this as unknown as Runners);
	}

	// npm uninstall.
	private async uninstallNpm(source: NpmSource, scope: SourceScope): Promise<void> {
		if (scope !== "user" && scope !== "project") {
			return (
				PiDefaultPackageManager.prototype as unknown as { uninstallNpm: DefaultPackageManager["uninstallNpm"] }
			).uninstallNpm.call(this, source, scope);
		}
		const self = this as unknown as { getNpmInstallRoot(s: SourceScope, t: boolean): string };
		const installRoot = self.getNpmInstallRoot(scope, false);
		if (!existsSync(installRoot)) {
			return;
		}
		const core = new CommandCore(
			"npmUninstall",
			JSON.stringify({ config: configJson(this), name: source.name, scope }),
		);
		await driveAsync(core, this as unknown as Runners);
	}

	// npm batch install (used by the per-scope update batches).
	private async installNpmBatch(specs: string[], scope: "user" | "project"): Promise<void> {
		const self = this as unknown as { getNpmInstallRoot(s: SourceScope, t: boolean): string; ensureNpmProject(r: string): void };
		const installRoot = self.getNpmInstallRoot(scope, false);
		self.ensureNpmProject(installRoot);
		const core = new CommandCore("npmInstall", JSON.stringify({ config: configJson(this), specs, scope }));
		await driveAsync(core, this as unknown as Runners);
	}

	// git fresh clone + optional checkout + git-dependency install. Reconcile of
	// an existing checkout still flows through the (inherited) update-target
	// resolution and the overridden ensureGitRef.
	private async installGit(source: GitSource, scope: SourceScope): Promise<void> {
		const self = this as unknown as {
			getGitInstallPath(s: GitSource, scope: SourceScope): string;
			getGitInstallRoot(scope: SourceScope): string | undefined;
			ensureGitIgnore(root: string): void;
			getLocalGitUpdateTarget(installedPath: string): Promise<{ ref: string; head: string; fetchArgs: string[] }>;
		};
		const targetDir = self.getGitInstallPath(source, scope);
		if (existsSync(targetDir)) {
			if (source.ref) {
				await this.ensureGitRef(targetDir, ["fetch", "origin", source.ref], "FETCH_HEAD");
				return;
			}
			const target = await self.getLocalGitUpdateTarget(targetDir);
			await this.ensureGitRef(targetDir, target.fetchArgs, target.ref);
			return;
		}
		const gitRoot = self.getGitInstallRoot(scope);
		if (gitRoot) {
			self.ensureGitIgnore(gitRoot);
		}
		mkdirSync(dirname(targetDir), { recursive: true });

		// Clone (+ checkout) with hasPackageJson:false so the machine stops before
		// the dependency install; pi checks package.json presence after the clone.
		const cloneCore = new CommandCore(
			"gitClone",
			JSON.stringify({
				config: configJson(this),
				repo: source.repo,
				targetDir,
				ref: source.ref ?? null,
				hasPackageJson: false,
			}),
		);
		await driveAsync(cloneCore, this as unknown as Runners);

		if (existsSync(join(targetDir, "package.json"))) {
			const depCore = new CommandCore(
				"gitDependencyInstall",
				JSON.stringify({ config: configJson(this), targetDir }),
			);
			await driveAsync(depCore, this as unknown as Runners);
		}
	}

	// Reconcile an existing checkout to `ref`: fetch, compare HEADs, and only on
	// a difference reset --hard / clean -fdx / reinstall git deps.
	private async ensureGitRef(targetDir: string, fetchArgs: string[], ref: string): Promise<void> {
		const hasPackageJson = existsSync(join(targetDir, "package.json"));
		const core = new CommandCore(
			"gitEnsureRef",
			JSON.stringify({ config: configJson(this), targetDir, fetchArgs, ref, hasPackageJson }),
		);
		await driveAsync(core, this as unknown as Runners);
	}

	// `npm root -g` (or bun `pm bin -g`) global-root probe, cached per npmCommand
	// exactly as pi does (so the "invalidate cached root" spy assertion holds).
	private getGlobalNpmRoot(): string {
		const self = this as unknown as {
			getNpmCommand(): { command: string; args: string[] };
			globalNpmRoot?: string;
			globalNpmRootCommandKey?: string;
		};
		const npmCommand = self.getNpmCommand();
		const commandKey = [npmCommand.command, ...npmCommand.args].join("\0");
		if (self.globalNpmRoot && self.globalNpmRootCommandKey === commandKey) {
			return self.globalNpmRoot;
		}
		const core = new CommandCore("npmGlobalRoot", JSON.stringify({ config: configJson(this) }));
		self.globalNpmRoot = driveSync(core, this as unknown as Runners) as string;
		self.globalNpmRootCommandKey = commandKey;
		return self.globalNpmRoot;
	}

	// `pnpm list -g --depth 0 --json` global-package path probe.
	private getPnpmGlobalPackagePath(packageName: string): string | undefined {
		const self = this as unknown as { getPackageManagerName(): string };
		if (self.getPackageManagerName() !== "pnpm") {
			return undefined;
		}
		const core = new CommandCore(
			"pnpmGlobalPath",
			JSON.stringify({ config: configJson(this), packageName }),
		);
		const result = driveSync(core, this as unknown as Runners);
		return result === null ? undefined : (result as string);
	}
}
