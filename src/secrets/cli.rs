//! `retaskable secret set|reset|list` and `retaskable-ci secret set|reset|list` (agent-settings.md section 3.3): saving a secret
//! from a terminal, for a computer with no browser to hand (a container). The value is asked for without echo at a
//! terminal, or read from the first line of stdin when piped, and never taken from an argument (it would sit in the
//! process list and the shell's history). The name is one of the program's own secrets or any other variable name the
//! rules allow (`names.rs`), and it is sealed under the
//! data key like a save on the settings page (a key file named by `RETASKABLE_SECRETS_KEY_FILE` is made on the first
//! save), and nothing fails open: with no key, nothing is saved. Touches no process environment and runs no `gh`; a
//! running program picks the saved value up as it does one saved on the page (the CI runner live, the code agent at
//! its next start).
use std::io::{BufRead, IsTerminal};
use std::path::Path;

use anyhow::{Result, bail};
use serde_json::{Map, Value};

use super::{Owner, Secrets, fingerprint, names};
use crate::env::Env;
use crate::link;

/// Runs the command in `args` (what follows `secret`) for `owner`'s secrets under the data folder `root`. The text to
/// print comes back.
pub fn run(owner: Owner, root: &Path, env: &Env, args: &[String]) -> Result<String> {
    let secrets = Secrets::for_page(owner, root, env, link::os_store(super::KEY_SERVICE), &[]);
    run_with(&secrets, owner, args, read_value)
}

pub(crate) fn run_with(
    secrets: &Secrets,
    owner: Owner,
    args: &[String],
    read: impl FnOnce(&str) -> Result<String>,
) -> Result<String> {
    let names: Vec<&str> = owner.defs().iter().map(|d| d.name).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["list"] => Ok(list(secrets)),
        ["set", name] => {
            // A name beyond the program's own is a variable the person adds: `plan` checks it under the same rules as the page.
            if !names.contains(name) {
                names::check_name(name).map_err(|e| anyhow::anyhow!("{e}"))?;
            }
            // Before asking for the value, so a computer that can't save never takes one.
            if let Some(why) = secrets.save_problem() {
                bail!("{why}");
            }
            let value = read(name)?;
            let value = value.trim();
            if value.is_empty() {
                bail!("No value given; nothing was saved.");
            }
            let mut change = Map::new();
            change.insert((*name).to_string(), Value::String(value.to_string()));
            let plan = secrets.plan(&change).map_err(|e| anyhow::anyhow!("{}", e.0))?;
            secrets.commit(plan)?;
            Ok(format!(
                "Saved {name} (fingerprint {}), sealed under this computer's data key.",
                fingerprint(value)
            ))
        }
        ["reset", name] => {
            // A name beyond the program's own is a variable the person adds: `plan` checks it under the same rules as the page.
            if !names.contains(name) {
                names::check_name(name).map_err(|e| anyhow::anyhow!("{e}"))?;
            }
            let mut change = Map::new();
            change.insert((*name).to_string(), Value::Null);
            secrets.commit(secrets.plan(&change).map_err(|e| anyhow::anyhow!("{}", e.0))?)?;
            Ok(format!(
                "Deleted the saved {name}: the environment's value is used if it sets one."
            ))
        }
        ["reset", _, ..] => bail!("`secret reset` takes only the name."),
        ["set", _, ..] => bail!("`secret set` takes only the name: the value is asked for, never an argument."),
        _ => bail!(
            "Usage: secret set <NAME> | secret reset <NAME> | secret list\nKnown names: {} (or any other variable name the rules allow).",
            names.join(", ")
        ),
    }
}

/// Each name, where its value comes from and its fingerprint; never a value.
fn list(secrets: &Secrets) -> String {
    secrets
        .states()
        .into_iter()
        .map(|s| {
            let source = s.source.as_str();
            match s.fingerprint {
                Some(print) => format!("{}  {source}  {print}", s.name),
                None => format!("{}  {source}", s.name),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Asked for without echo at a terminal; else the first line of stdin.
fn read_value(name: &str) -> Result<String> {
    if std::io::stdin().is_terminal() {
        return Ok(rpassword::prompt_password(format!("{name}: "))?);
    }
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line)
}

#[cfg(test)]
mod tests;
