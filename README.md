# Dex

Dex is a desktop multi-agent orchestrator for **Claude Code**. A leader delegates work to workers; you follow their terminals, task board, plans and results in one app. The UI and bundled agent instructions are currently **Turkish**.

Türkçe not: Dex, Claude Code ajanlarını lider ve worker takımlarıyla yönetir. Arayüz Türkçedir; birincil platform Windows'tur.

## What it does

- Teams with leaders and workers, a task board, delegation and dependency plans ([delegation](crates/dex-core/src/delegate.rs), [plans](crates/dex-core/src/plan.rs)).
- Git worktrees for delegated jobs and a merge review gate when the team has a code-review member. Without that role, there is no review gate ([worktrees](crates/dex-core/src/worktree.rs), [review](crates/dex-core/src/review.rs)).
- Hook policies for tool use and job lifecycle, plus persistent project/team/agent memory ([hooks](crates/dex-core/src/hooks.rs), [policy](crates/dex-core/src/permit.rs), [memory](crates/dex-core/src/memory.rs)).
- A provider chain for quota exhaustion and proactive Claude worker quota switching ([chain](crates/dex-core/src/chain.rs), [quota](crates/dex-core/src/quota.rs)).

## Status and platforms

Early development, version 0.1.0 ([manifest](package.json)). CI checks builds and unit tests; it does not establish desktop feature parity or signed release readiness.

| Platform | Status | Scope |
| --- | --- | --- |
| Windows | Primary | Desktop; CI build and unit-test jobs |
| macOS | Experimental | CI build and unit-test job; no process panel or screen capture |
| Linux | Experimental | Not in the current CI; no process panel or screen capture |
| Android | Experimental, optional | Remote client; requires a paired dexd host and separate build/signing setup |

Sources: [CI](.github/workflows/ci.yml), [process panel](crates/dex-core/src/proc.rs), [capture](src-tauri/src/snip.rs), [mobile client](src/mobile/MobileApp.tsx).

## Windows quick start

Install these before building:

| Requirement | Version / setup |
| --- | --- |
| Git | Available on PATH; Git for Windows/Git Bash for Claude Code's Bash tool ([CLI setup](https://code.claude.com/docs/en/setup)) |
| Node.js | **24.14.1**, pinned in [.nvmrc](.nvmrc) |
| pnpm | **10.17.0**, pinned in [package.json](package.json) |
| Rust via rustup | **1.96.0**, pinned in [rust-toolchain.toml](rust-toolchain.toml); use the Windows MSVC toolchain |
| Native build tools | Microsoft C++ Build Tools, **Desktop development with C++**, Windows SDK and WebView2 Runtime; follow [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/) |
| Claude Code | Install separately and complete login before launching agents |
| curl | **8.3+** on the hook process PATH, for curl's environment expansion |

CLI/hook requirements: [Claude launch](crates/dex-core/src/pty.rs), [curl hooks](crates/dex-core/src/hooks.rs). Neither the CLI nor its subscription is included with Dex.

From a checkout of this repository:

```powershell
pnpm install
pnpm tauri dev
```

The dev command builds local `dexd` before starting the desktop UI ([Tauri config](src-tauri/tauri.conf.json)). No remote server is needed. In the Turkish UI, configure your project/team and start its leader; Claude Code login is separate.

For a distributable desktop package:

```powershell
pnpm release
```

**Use `pnpm release`: plain `pnpm tauri build` does not prepare or include the required dexd sidecar.** The release script builds it and applies the [sidecar config](src-tauri/tauri.release.conf.json) ([script](scripts/dexd-sidecar.mjs), [package scripts](package.json)).

## Optional modules

| Module | Setup and behavior without it |
| --- | --- |
| Remote dexd / tailnet | Pair your own host; remote hosts start empty and tailnet is disabled by default. Local desktop work continues without either. Tailnet needs Tailscale. See [self-hosting](docs/deploy.md), [host registry](src/api/hosts.ts), [tailnet](crates/dexd/src/tailnet.rs). |
| GPU broker | Configure broker addresses and credentials. Without them, GPU operations are unavailable; normal local agent work continues. Submissions require an approved job grant and budget ([GPU](crates/dex-core/src/gpu.rs)). |
| Usage strip | dexd measures Claude/GLM/Muse 5-hour and weekly quota windows and today's agent tokens. A provider without credentials or a readable quota shows "--"; agents run regardless ([usage](crates/dex-core/src/usage.rs)). |
| Browser tools | Browser-enabled roles use Node/npx and Playwright MCP 0.0.79. Windows uses Edge, macOS Chrome, Linux Playwright Chromium. Missing browser/package access breaks those tools; other roles do not require them ([browser](crates/dex-core/src/browser.rs)). |
| Android | Requires a paired host. `pnpm release:apk` is a separate SSH/SDK/NDK/keystore pipeline; configure it before use. `pnpm release:all` also runs that pipeline ([APK script](scripts/release-apk.mjs)). |

## Data and network

Desktop data and config use Tauri's OS application directories for the bundle identifier. On Windows these are under `%APPDATA%`; Core stores `dex.db` and `logs/` there. Project memory/worktrees/results also use the project's `.dex/` tree; team and agent memory depend on the selected project/team scope. Credentials use the OS secret store ([Tauri directories](https://v2.tauri.app/reference/javascript/api/namespacepath/), [desktop paths](src-tauri/src/core_proc.rs), [Core paths](crates/dex-core/src/paths.rs), [project scope](crates/dex-core/src/project_context.rs), [secrets](crates/dex-core/src/secrets.rs)).

Dex has no analytics or telemetry collector. It does make authenticated model catalogue and quota requests; optional services and agent actions make additional connections. See [network behavior](docs/network.md) before use.

## Security

**Agents run with broad access by default.** Claude uses `bypassPermissions`. Agents can act with your OS user's file, command and network privileges ([Claude launch](crates/dex-core/src/pty.rs)).

Dex's hooks apply role/job policies and the Core controls delegation, GPU grants and applicable merge reviews. This is application policy, **not an OS sandbox or complete containment boundary**. Worktrees isolate Git work, not the host. Use an isolated account or machine for untrusted repositories and tasks. Report vulnerabilities through [SECURITY.md](SECURITY.md).

## Contributing and support

For contributors, install the repository Git hook:

```powershell
npm run hooks:install
```

Run it before your first contribution. It downloads and verifies gitleaks **8.30.0** and installs the pre-commit and commit-msg hooks; a missing or invalid gitleaks binary blocks commits. `pnpm install` does not install these hooks. CI also scans the tree and history ([installer](scripts/install-hooks.mjs), [hook](scripts/hooks/pre-commit), [CI](.github/workflows/ci.yml)). These Git hooks are separate from runtime agent hooks.

Open an issue for reproducible bugs or focused feature requests. Include Dex/CLI versions, OS, reproduction steps and redacted errors. Remove credentials, prompts, personal paths and host details. Use private reporting for security issues. Support is best effort; Dex is provided **as is, without warranty**.

Dex code is [MIT licensed](LICENSE), Copyright (c) 2026 hakunokina. See [NOTICE](NOTICE) for third-party material, generated office artwork and trademarks.
