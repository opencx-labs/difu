//! Extract identifiable test failures from GitHub Actions logs; never infer tests
//! from generic build/lint errors. Other providers retain their original link.
use crate::{
    github,
    model::{Check, PrKey},
    process::{self, Cancel},
};
use anyhow::{Context, Result, ensure};

#[derive(Clone, Debug)]
pub struct FailedTest {
    pub name: String,
    pub excerpt: Vec<String>,
}
#[derive(Clone, Debug, Default)]
pub struct Failures {
    pub tests: Vec<FailedTest>,
    pub explanation: Option<String>,
}

pub fn load(key: &PrKey, check: &Check, cancel: &Cancel) -> Failures {
    match read(key, check, cancel) {
        Ok(failures) => failures,
        Err(error) => Failures {
            explanation: Some(format!(
                "Could not read test failures: {error:#}. Open the failed check for details."
            )),
            ..Failures::default()
        },
    }
}
fn read(key: &PrKey, check: &Check, cancel: &Cancel) -> Result<Failures> {
    key.validate()?;
    let job = actions_job(key, &check.url)?;
    let output = process::run_limited(
        github::command().args([
            "api",
            &format!("repos/{}/actions/jobs/{job}/logs", key.repository()),
        ]),
        cancel,
        16 * 1024 * 1024,
    )?;
    ensure!(
        output.code == 0,
        "{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let text = String::from_utf8(output.stdout).context("Job log is not text")?;
    Ok(parse(&text))
}
fn actions_job(key: &PrKey, value: &str) -> Result<u64> {
    let url = url::Url::parse(value).context("No GitHub Actions job log link is available")?;
    ensure!(
        url.scheme() == "https" && url.host_str() == Some("github.com"),
        "This check uses another provider"
    );
    let parts = url
        .path_segments()
        .context("Invalid check link")?
        .collect::<Vec<_>>();
    let [owner, repo, "actions", "runs", run, "job", job] = parts.as_slice() else {
        anyhow::bail!("This check does not expose a GitHub Actions job log");
    };
    ensure!(
        owner.eq_ignore_ascii_case(&key.owner) && repo.eq_ignore_ascii_case(&key.repo),
        "The check links to a different repository"
    );
    run.parse::<u64>()?;
    let id = job.parse::<u64>()?;
    ensure!(id > 0, "Invalid Actions job ID");
    Ok(id)
}
fn clean_line(line: &str) -> String {
    let mut output = String::new();
    let mut escape = false;
    let mut sequence = false;
    for c in line.chars() {
        if escape {
            if !sequence && c == '[' {
                sequence = true;
                continue;
            }
            if !sequence || ('@'..='~').contains(&c) {
                escape = false;
                sequence = false;
            }
            continue;
        }
        if c == '\u{1b}' {
            escape = true;
            continue;
        }
        if c == '\t' || !c.is_control() {
            output.push(c);
        }
    }
    let text = output.trim();
    let text = text
        .split_once(' ')
        .filter(|(prefix, _)| chrono::DateTime::parse_from_rfc3339(prefix).is_ok())
        .map_or(text, |(_, rest)| rest.trim());
    text.trim_start_matches("##[error]")
        .trim()
        .chars()
        .take(500)
        .collect()
}
fn test_name(line: &str) -> Option<String> {
    if let Some(test) = line
        .strip_prefix("test ")
        .and_then(|s| s.strip_suffix(" ... FAILED"))
    {
        return Some(test.to_owned());
    }
    // Pytest's short test summary explicitly identifies a test node.
    if let Some(rest) = line.strip_prefix("FAILED ")
        && rest.contains("::")
    {
        return Some(rest.split(" - ").next().unwrap_or(rest).to_owned());
    }
    // Jest/Vitest test case diagnostics, including their suite hierarchy.
    for prefix in ["FAIL  ", "FAIL ", "● ", "× ", "✕ ", "✖ ", "❯ "] {
        if let Some(rest) = line.strip_prefix(prefix) {
            let rest = rest.trim();
            // A bare FAIL filename is a suite, not an individual test case.
            if !rest.is_empty()
                && (!(prefix.starts_with("FAIL") || prefix == "❯ ") || rest.contains(" > "))
                && !rest.starts_with("Test Files")
                && !rest.starts_with("Tests ")
            {
                return Some(rest.to_owned());
            }
        }
    }
    None
}
pub(crate) fn parse(log: &str) -> Failures {
    let mut tests: Vec<FailedTest> = Vec::new();
    let mut current = None;
    let mut extra = false;
    for line in log.lines().map(clean_line) {
        if let Some(name) = line
            .strip_prefix("---- ")
            .and_then(|s| s.strip_suffix(" stdout ----"))
        {
            current = tests.iter().position(|t| t.name == name);
            if let Some(test) = current.and_then(|i| tests.get_mut(i)) {
                test.excerpt.clear();
            }
            continue;
        }
        if let Some(name) = test_name(&line) {
            current = tests.iter().position(|t| t.name == name);
            if current.is_none() {
                if tests.len() >= 50 {
                    extra = true;
                    continue;
                }
                tests.push(FailedTest {
                    name,
                    excerpt: Vec::new(),
                });
                current = tests.len().checked_sub(1);
            }
        } else if let Some(test) = current.and_then(|i| tests.get_mut(i))
            && !line.is_empty()
            && test.excerpt.len() < 6
            && !line.starts_with("##[")
        {
            test.excerpt.push(line);
        }
    }
    let explanation = if tests.is_empty() {
        Some("No individual test failures could be identified in this log. It may be a build/lint failure or an unsupported test format; open the failed check for details.".into())
    } else if extra {
        Some("Showing the first 50 test failures; open the failed check for the full log.".into())
    } else {
        None
    };
    Failures { tests, explanation }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extracts_test_diagnostics_without_inventing_tests_from_build_errors() {
        let logs = "2026-09-16T13:10:00Z \u{1b}[31m FAIL  src/mail.spec.ts > Mail > sends a message\u{1b}[0m\n2026-09-16T13:10:01Z AssertionError: expected 2 to equal 1\ntest service::test_reply ... FAILED\nassertion left == right failed\nFAILED tests/test_mail.py::test_reply - AssertionError\n● Inbox › displays sender\nExpected visible sender\n";
        let parsed = parse(logs);
        assert_eq!(parsed.tests.len(), 4);
        assert_eq!(
            parsed.tests.first().map(|t| t.name.as_str()),
            Some("src/mail.spec.ts > Mail > sends a message")
        );
        assert!(
            parsed
                .tests
                .first()
                .is_some_and(|t| t.excerpt.iter().any(|s| s.contains("AssertionError")))
        );
        assert!(
            parse("error TS2345: invalid type\nProcess completed with exit code 2")
                .tests
                .is_empty()
        );
        assert!(parse("FAIL src/file.spec.ts").tests.is_empty());
    }
    #[test]
    fn refuses_unrelated_log_links() -> Result<()> {
        let key = PrKey {
            owner: "example".into(),
            repo: "project".into(),
            number: 1,
        };
        assert_eq!(
            actions_job(
                &key,
                "https://github.com/example/project/actions/runs/12/job/34"
            )?,
            34
        );
        assert!(
            actions_job(
                &key,
                "https://other.test/example/project/actions/runs/12/job/34"
            )
            .is_err()
        );
        assert!(
            actions_job(
                &key,
                "https://github.com/else/project/actions/runs/12/job/34"
            )
            .is_err()
        );
        Ok(())
    }
}
