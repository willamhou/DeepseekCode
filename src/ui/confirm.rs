use std::io::{self, BufRead, IsTerminal, Write};

pub fn confirm(prompt: &str) -> bool {
    if !io::stdin().is_terminal() {
        let mut stderr = io::stderr().lock();
        let _ = writeln!(
            stderr,
            "[non-interactive] auto-denying: {prompt} (set DSCODE_AUTO_APPROVE_WRITES=1 / DSCODE_AUTO_APPROVE_SHELL=1 to bypass)"
        );
        let _ = stderr.flush();
        return false;
    }

    let mut stderr = io::stderr().lock();
    let _ = write!(stderr, "{prompt} [y/N]: ");
    let _ = stderr.flush();
    drop(stderr);

    let mut buffer = String::new();
    if io::stdin().lock().read_line(&mut buffer).is_err() {
        return false;
    }
    parse_confirmation(&buffer)
}

pub fn parse_confirmation(input: &str) -> bool {
    matches!(
        input.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    )
}

#[cfg(test)]
mod tests {
    use super::parse_confirmation;

    #[test]
    fn accepts_explicit_yes_answers() {
        assert!(parse_confirmation("y"));
        assert!(parse_confirmation("Y"));
        assert!(parse_confirmation("yes"));
        assert!(parse_confirmation("YES"));
        assert!(parse_confirmation("  y  \n"));
        assert!(parse_confirmation("Yes\r\n"));
    }

    #[test]
    fn rejects_blank_or_negative_answers() {
        assert!(!parse_confirmation(""));
        assert!(!parse_confirmation("\n"));
        assert!(!parse_confirmation("n"));
        assert!(!parse_confirmation("no"));
        assert!(!parse_confirmation("maybe"));
        assert!(!parse_confirmation("yeah"));
    }
}
