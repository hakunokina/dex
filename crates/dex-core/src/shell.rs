//! DEX-29: the patron's own shell in a Dex pane. Not an agent: `pty::spawn` gives it no hooks,
//! MCP, memory, role, provider env or CPU job — only the PTY. The webview asks for `cmd = "shell"`
//! and the core picks the platform's shell, so the UI never names a binary.

pub const SHELL_CMD: &str = "shell";

/// Windows: `pwsh` (PowerShell 7) when it is on PATH, else the built-in `powershell.exe`.
/// Elsewhere: `$SHELL`, falling back to `/bin/sh`.
pub fn resolve() -> (String, Vec<String>) {
    #[cfg(windows)]
    {
        let exe = if on_path("pwsh.exe") { "pwsh.exe" } else { "powershell.exe" };
        (exe.to_string(), vec!["-NoLogo".to_string()])
    }
    #[cfg(not(windows))]
    {
        let sh = std::env::var("SHELL").ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "/bin/sh".into());
        (sh, Vec::new())
    }
}

#[cfg(windows)]
fn on_path(exe: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(exe).is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_to_a_real_shell() {
        let (cmd, args) = resolve();
        #[cfg(windows)]
        {
            assert!(cmd == "pwsh.exe" || cmd == "powershell.exe", "{cmd}");
            assert_eq!(args, ["-NoLogo"]);
            // powershell.exe ships with Windows; pwsh only when found on PATH.
            if cmd == "pwsh.exe" {
                assert!(on_path("pwsh.exe"));
            }
        }
        #[cfg(not(windows))]
        {
            assert!(!cmd.is_empty());
            assert!(args.is_empty());
        }
    }
}
