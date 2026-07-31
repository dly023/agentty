use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeInvocation {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
}

impl ResumeInvocation {
    pub fn new(program: impl Into<String>, args: Vec<String>, cwd: Option<String>) -> Option<Self> {
        let program = program.into();
        if program.is_empty()
            || program.bytes().any(|b| b == 0)
            || args.iter().any(|arg| arg.bytes().any(|b| b == 0))
        {
            return None;
        }
        Some(Self { program, args, cwd })
    }
}

/// Materialize a typed invocation at the final interactive-shell boundary.
/// The returned bytes include carriage return for terminal submission.
pub fn shell_line(invocation: &ResumeInvocation) -> Vec<u8> {
    let mut rendered = shell_word(&invocation.program);
    for arg in &invocation.args {
        rendered.push(' ');
        rendered.push_str(&shell_word(arg));
    }
    rendered.push('\r');
    rendered.into_bytes()
}

fn shell_word(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&byte))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_resume_keeps_program_and_arguments_separate() {
        let invocation = ResumeInvocation::new(
            "codex",
            vec!["resume".into(), "session with spaces".into()],
            Some("/repo with spaces".into()),
        )
        .unwrap();
        assert_eq!(invocation.program, "codex");
        assert_eq!(invocation.args, ["resume", "session with spaces"]);
        assert_eq!(invocation.cwd.as_deref(), Some("/repo with spaces"));
    }

    #[test]
    fn shell_materialization_quotes_each_argument_at_the_terminal_boundary() {
        let invocation = ResumeInvocation::new(
            "codex",
            vec![
                "resume".into(),
                "session with spaces".into(),
                "o'brien".into(),
            ],
            None,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(shell_line(&invocation)).unwrap(),
            "codex resume 'session with spaces' 'o'\\''brien'\r"
        );
    }

    #[test]
    fn shell_materialization_does_not_execute_argument_metacharacters() {
        let invocation = ResumeInvocation::new(
            "codex",
            vec!["resume".into(), "$(touch /tmp/pwned); rm -rf ~".into()],
            None,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(shell_line(&invocation)).unwrap(),
            "codex resume '$(touch /tmp/pwned); rm -rf ~'\r"
        );
    }
}
