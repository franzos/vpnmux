use crate::types::Result;
#[cfg(test)]
use std::cell::RefCell;
#[cfg(test)]
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct CmdOutput {
    pub code: i32,
    pub stdout: String,
}

impl CmdOutput {
    pub fn ok(&self) -> bool {
        self.code == 0
    }
}

/// All external command execution goes through this seam so reconcile logic
/// is testable without touching the real system.
pub trait Runner {
    fn run(&self, bin: &str, args: &[&str]) -> Result<CmdOutput>;

    /// Like `run`, but feeds `stdin` to the child. Default delegates to `run`
    /// (ignoring stdin) so implementors only override when they need it.
    fn run_stdin(&self, bin: &str, args: &[&str], _stdin: &str) -> Result<CmdOutput> {
        self.run(bin, args)
    }
}

pub struct RealRunner;

impl Runner for RealRunner {
    fn run(&self, bin: &str, args: &[&str]) -> Result<CmdOutput> {
        let out = std::process::Command::new(bin).args(args).output()?;
        let code = out.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        crate::debug!("exec: {} {} -> {}", bin, args.join(" "), code);
        if code != 0 && !stderr.trim().is_empty() {
            crate::debug!("exec stderr: {}", stderr.trim());
        }
        Ok(CmdOutput {
            code,
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        })
    }

    fn run_stdin(&self, bin: &str, args: &[&str], stdin: &str) -> Result<CmdOutput> {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut child = Command::new(bin)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .ok_or("run_stdin: child stdin unavailable")?
            .write_all(stdin.as_bytes())?;
        let out = child.wait_with_output()?;
        let code = out.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        crate::debug!("exec: {} {} <stdin> -> {}", bin, args.join(" "), code);
        if code != 0 && !stderr.trim().is_empty() {
            crate::debug!("exec stderr: {}", stderr.trim());
        }
        Ok(CmdOutput {
            code,
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        })
    }
}

/// Test double: returns scripted output keyed by the full "bin arg1 arg2"
/// command line, records every call, and returns exit 0 / empty for unknowns.
#[cfg(test)]
pub struct MockRunner {
    responses: HashMap<String, CmdOutput>,
    pub calls: RefCell<Vec<String>>,
}

#[cfg(test)]
impl MockRunner {
    pub fn new() -> Self {
        MockRunner {
            responses: HashMap::new(),
            calls: RefCell::new(Vec::new()),
        }
    }

    /// Register a canned response for an exact command line.
    pub fn on(mut self, cmdline: &str, code: i32, stdout: &str) -> Self {
        self.responses.insert(
            cmdline.to_string(),
            CmdOutput {
                code,
                stdout: stdout.to_string(),
            },
        );
        self
    }

    pub fn called(&self, cmdline: &str) -> bool {
        self.calls.borrow().iter().any(|c| c == cmdline)
    }
}

#[cfg(test)]
impl Runner for MockRunner {
    fn run(&self, bin: &str, args: &[&str]) -> Result<CmdOutput> {
        let key = std::iter::once(bin)
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");
        self.calls.borrow_mut().push(key.clone());
        Ok(self.responses.get(&key).cloned().unwrap_or(CmdOutput {
            code: 0,
            stdout: String::new(),
        }))
    }

    fn run_stdin(&self, bin: &str, args: &[&str], _stdin: &str) -> Result<CmdOutput> {
        self.run(bin, args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_returns_scripted_output_and_records_calls() {
        let r = MockRunner::new().on("mullvad status", 0, "Connected\n");
        let out = r.run("mullvad", &["status"]).unwrap();
        assert!(out.ok());
        assert!(out.stdout.starts_with("Connected"));
        assert!(r.called("mullvad status"));
    }

    #[test]
    fn mock_unknown_command_is_success_empty() {
        let r = MockRunner::new();
        let out = r.run("nft", &["list", "table", "inet", "mullvad"]).unwrap();
        assert!(out.ok());
        assert_eq!(out.stdout, "");
        assert!(r.called("nft list table inet mullvad"));
    }
}
