//! Login-keychain secrets through Apple's `security` tool. macOS ties an item's
//! "Always Allow" to the program that reads it, and difu's own signature
//! changes with every build, so items difu read directly asked for the login
//! password again after each update. Items written and read by `security`
//! trust that stable system tool instead.
use anyhow::{Context, Result, ensure};
use std::{
    io::Write,
    process::{Command, Stdio},
};

const SECURITY: &str = "/usr/bin/security";

/// Names travel on a `security` command line, so they may not start an option.
fn plain(value: &str) -> bool {
    value
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphanumeric())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// Creates or replaces an item. The secret is passed on stdin, never as a
/// process argument.
pub fn save(service: &str, account: &str, secret: &[u8]) -> Result<()> {
    ensure!(
        plain(service) && plain(account),
        "Invalid keychain item name"
    );
    let hex: String = secret.iter().map(|byte| format!("{byte:02x}")).collect();
    let mut child = Command::new(SECURITY)
        .arg("-i")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("Cannot run macOS security")?;
    let mut input = child.stdin.take().context("Missing security input")?;
    let written = input.write_all(
        format!("add-generic-password -U -s {service} -a {account} -X {hex}\n").as_bytes(),
    );
    drop(input);
    let status = child.wait()?;
    written.context("Cannot pass the secret to macOS security")?;
    ensure!(status.success(), "Cannot save the secret in macOS Keychain");
    Ok(())
}

pub fn read(service: &str, account: &str) -> Result<Vec<u8>> {
    ensure!(
        plain(service) && plain(account),
        "Invalid keychain item name"
    );
    let output = Command::new(SECURITY)
        .args(["find-generic-password", "-s", service, "-a", account, "-w"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .context("Cannot run macOS security")?;
    ensure!(
        output.status.success(),
        "The secret is missing from macOS Keychain"
    );
    let mut text = output.stdout;
    if text.last() == Some(&b'\n') {
        text.pop();
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_restricted_to_safe_characters() {
        assert!(plain("app.difu.slack"));
        assert!(plain("0a9f_account-1"));
        for name in ["", "two words", "-X", "a;b", "a\nb", "quote\""] {
            assert!(!plain(name), "{name:?}");
        }
        assert!(save("bad service", "a", b"x").is_err());
        assert!(read("a", "bad\naccount").is_err());
    }
}
