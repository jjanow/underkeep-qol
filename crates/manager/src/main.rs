//! `uk-qol`: installs, configures and updates Underkeep QoL.
//!
//! Only the command skeleton exists so far; see PLAN.md section 7.

use std::process::ExitCode;

const COMMANDS: &[(&str, &str)] = &[
    ("detect", "find Underkeep installs"),
    ("install", "install the mod loader into a game folder"),
    ("status", "show the game build and which mods work on it"),
    ("enable", "enable a mod"),
    ("disable", "disable a mod"),
    ("config", "show or change a mod setting"),
    ("uninstall", "remove the mod loader"),
    ("report", "print a report to attach to bug reports"),
];

fn usage() -> String {
    let mut s = format!(
        "uk-qol {}\n\nUsage: uk-qol <command>\n\nCommands:\n",
        env!("CARGO_PKG_VERSION")
    );
    for (name, help) in COMMANDS {
        s.push_str(&format!("  {name:<10} {help}\n"));
    }
    s
}

fn main() -> ExitCode {
    let arg = std::env::args().nth(1);
    match arg.as_deref() {
        None | Some("-h" | "--help" | "help") => {
            print!("{}", usage());
            ExitCode::SUCCESS
        }
        Some("-V" | "--version") => {
            println!("uk-qol {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some(cmd) if COMMANDS.iter().any(|(n, _)| *n == cmd) => {
            eprintln!("uk-qol: `{cmd}` is not implemented yet");
            ExitCode::FAILURE
        }
        Some(cmd) => {
            eprint!("uk-qol: unknown command `{cmd}`\n\n{}", usage());
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_lists_every_command() {
        let u = usage();
        for (name, _) in COMMANDS {
            assert!(u.contains(name));
        }
    }
}
