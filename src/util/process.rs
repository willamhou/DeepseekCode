use std::process::Command;

use crate::error::{app_error, tool_failure, AppResult};

#[derive(Debug, Clone)]
pub struct CapturedOutput {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

pub fn run_capture(bin: &str, args: &[&str]) -> AppResult<CapturedOutput> {
    let output = Command::new(bin).args(args).output().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            app_error(format!(
                "{bin} not found in PATH; install it before retrying"
            ))
        } else {
            app_error(format!("could not invoke {bin}: {error}"))
        }
    })?;
    Ok(CapturedOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        success: output.status.success(),
    })
}

pub fn run_capture_stdout(bin: &str, args: &[&str]) -> AppResult<String> {
    let captured = run_capture(bin, args)?;
    if !captured.success {
        return Err(tool_failure(format!(
            "{bin} {} failed: {}",
            args.first().copied().unwrap_or(""),
            captured.stderr.trim()
        )));
    }
    Ok(captured.stdout)
}

use std::io::BufReader;
use std::process::{ChildStdout, ExitStatus, Stdio};

pub struct StreamingProcess {
    child: std::process::Child,
    pub stdout: BufReader<ChildStdout>,
}

pub fn spawn_streaming(bin: &str, args: &[&str]) -> AppResult<StreamingProcess> {
    let mut child = Command::new(bin)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                app_error(format!("{bin} not found in PATH; install it before retrying"))
            } else {
                app_error(format!("could not invoke {bin}: {error}"))
            }
        })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| app_error(format!("{bin} produced no stdout pipe")))?;
    Ok(StreamingProcess {
        child,
        stdout: BufReader::new(stdout),
    })
}

impl StreamingProcess {
    pub fn finish(mut self) -> AppResult<(ExitStatus, String)> {
        use std::io::Read;
        let mut sink = Vec::new();
        let _ = self.stdout.read_to_end(&mut sink);

        let mut stderr_buf = String::new();
        if let Some(mut stderr) = self.child.stderr.take() {
            let _ = stderr.read_to_string(&mut stderr_buf);
        }
        let status = self
            .child
            .wait()
            .map_err(|error| app_error(format!("failed to await child: {error}")))?;
        const TAIL_LIMIT: usize = 64 * 1024;
        let tail = if stderr_buf.len() > TAIL_LIMIT {
            stderr_buf.split_off(stderr_buf.len() - TAIL_LIMIT)
        } else {
            stderr_buf
        };
        Ok((status, tail))
    }
}

impl Drop for StreamingProcess {
    fn drop(&mut self) {
        // try_wait first — if the child already exited (e.g., finish() was called),
        // skip the kill to avoid noisy errors.
        if let Ok(Some(_)) = self.child.try_wait() {
            return;
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_capture_stdout_returns_stdout_for_successful_command() {
        let stdout = run_capture_stdout("echo", &["hello"]).unwrap();
        assert!(stdout.starts_with("hello"));
    }

    #[test]
    fn run_capture_stdout_errors_when_command_returns_nonzero() {
        let result = run_capture_stdout("sh", &["-c", "exit 7"]);
        assert!(result.is_err());
    }

    #[test]
    fn run_capture_returns_not_found_error_for_missing_binary() {
        let result = run_capture("definitely-not-a-real-binary-xyz", &[]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found in PATH"));
    }
}
