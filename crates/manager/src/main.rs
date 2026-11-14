//! `uk-qol`: installs, configures and updates Underkeep QoL. One program for
//! both the command line and the desktop app.
//!
//! Started with a command (`uk-qol status`), it runs that command. Started
//! with none, it opens the app, unless it was started from a terminal, where
//! it prints help instead (`uk-qol gui` opens the app from a terminal). On
//! Windows a double-click opens a console window for a moment before the app
//! hides it: the program is a console program so that commands typed in
//! `cmd` or PowerShell print their output.

mod cli;
mod config;
mod gui;
mod i18n;
mod install;
mod launch;
mod mods;
mod paths;
mod process;
mod report;
mod status;
mod update;
mod vdf;

use std::process::ExitCode;

/// Started by double-click (Explorer) rather than from a terminal: the
/// console window was created for this process alone. Hides it.
#[cfg(windows)]
fn detach_own_console() -> bool {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetConsoleProcessList(list: *mut u32, count: u32) -> u32;
        fn FreeConsole() -> i32;
    }
    let mut pids = [0u32; 2];
    // SAFETY: the buffer is valid for `count` entries.
    let n = unsafe { GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) };
    if n == 1 {
        // SAFETY: plain Win32 call without arguments.
        unsafe { FreeConsole() };
        true
    } else {
        false
    }
}

fn main() -> ExitCode {
    update::cleanup_leftovers();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match argv.first().map(String::as_str) {
        None => {
            #[cfg(windows)]
            let gui = detach_own_console();
            #[cfg(not(windows))]
            let gui = {
                use std::io::IsTerminal;
                !std::io::stdin().is_terminal()
            };
            if gui {
                return gui::run(None);
            }
            print!("{}", cli::usage());
            ExitCode::SUCCESS
        }
        Some("-h" | "--help" | "help") => {
            print!("{}", cli::usage());
            ExitCode::SUCCESS
        }
        Some("-V" | "--version") => {
            println!("uk-qol {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some(_) => match cli::parse(&argv) {
            Ok(a) if a.cmd == "gui" => gui::run(a.game),
            Ok(a) => cli::run(a),
            Err(e) => {
                eprint!("uk-qol: {e}\n\n{}", cli::usage());
                ExitCode::from(2)
            }
        },
    }
}
