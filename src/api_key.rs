//! The Gemini API key: asked for in a native dialog, kept in the login
//! Keychain, read by the summarizer.
//!
//! Not in the config file, which sits next to the recordings and is easy to
//! share by accident, and not baked in at build time: the repo is public and
//! the binary would carry it. An app opened from Finder doesn't inherit shell
//! variables, so the Keychain is what it actually reads. Every access goes
//! through `/usr/bin/security`, which owns the item it creates, so a rebuilt
//! (re-signed) app never triggers a Keychain permission prompt.

use anyhow::{Context, Result};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

const SERVICE: &str = "stt-md";
const ACCOUNT: &str = "gemini";

/// `GEMINI_API_KEY` if set, else the Keychain item.
pub fn gemini_key() -> Option<String> {
    if let Ok(key) = std::env::var("GEMINI_API_KEY")
        && !key.trim().is_empty()
    {
        return Some(key.trim().to_string());
    }
    let out = Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", SERVICE, "-a", ACCOUNT, "-w"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let key = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !key.is_empty()).then_some(key)
}

/// Stores the key, replacing any previous one. The key goes through stdin of
/// `security -i`, not its arguments, so it never shows up in `ps`.
pub fn save_gemini_key(key: &str) -> Result<()> {
    anyhow::ensure!(
        is_plausible_key(key),
        "la key tiene caracteres que no corresponden a una API key de Gemini"
    );
    let mut child = Command::new("/usr/bin/security")
        .arg("-i")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to run security")?;
    writeln!(
        child.stdin.take().context("no stdin for security")?,
        "add-generic-password -U -s {SERVICE} -a {ACCOUNT} -l \"stt-md Gemini API key\" -w {key}"
    )?;
    let out = child.wait_with_output()?;
    let err = String::from_utf8_lossy(&out.stderr);
    anyhow::ensure!(out.status.success() && err.trim().is_empty(), "security: {}", err.trim());
    Ok(())
}

pub fn delete_gemini_key() -> Result<()> {
    let out = Command::new("/usr/bin/security")
        .args(["delete-generic-password", "-s", SERVICE, "-a", ACCOUNT])
        .output()?;
    anyhow::ensure!(out.status.success(), "no había una key guardada");
    Ok(())
}

/// Gemini keys are `AIza` plus URL-safe characters; anything else (a pasted
/// sentence, a quote) would also break the `security -i` command line.
fn is_plausible_key(key: &str) -> bool {
    (20..=100).contains(&key.len())
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Asks Gemini to list its models with this key: proves it works before it is
/// trusted with a meeting.
pub fn validate_gemini_key(key: &str) -> Result<()> {
    let resp = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()?
        .get("https://generativelanguage.googleapis.com/v1beta/models?pageSize=1")
        .header("x-goog-api-key", key)
        .send()
        .context("no se pudo conectar con Gemini")?;
    let status = resp.status();
    anyhow::ensure!(status.is_success(), "Gemini rechazó la key ({status})");
    Ok(())
}

/// Native dialog with a hidden text field. `None` when cancelled.
pub fn ask_for_gemini_key() -> Option<String> {
    let script = r#"display dialog "Pega tu API key de Gemini. Se guarda en el Llavero de macOS y se usa para resumir las reuniones; si falla, stt-md resume con el modelo local." with title "stt-md" default answer "" with hidden answer buttons {"Cancelar", "Guardar"} default button "Guardar" cancel button "Cancelar"
text returned of result"#;
    let out = Command::new("osascript").args(["-e", script]).output().ok()?;
    let key = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !key.is_empty()).then_some(key)
}

pub fn show_message(title: &str, message: &str) {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        "display dialog \"{}\" with title \"{}\" buttons {{\"OK\"}} default button \"OK\"",
        esc(message),
        esc(title)
    );
    let _ = Command::new("osascript").args(["-e", &script]).output();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plausible_keys_are_url_safe_tokens() {
        assert!(is_plausible_key("AIzaSyA-fake_key_0123456789abcdefghij"));
        assert!(!is_plausible_key("AIza fake key with spaces 0123456789"));
        assert!(!is_plausible_key("AIza\"; delete-keychain x"));
        assert!(!is_plausible_key("short"));
    }
}
