//! Puts the git commit the runtime was built from into `UK_QOL_COMMIT`, so
//! the log tells two builds of the same version apart. CI's `GITHUB_SHA`
//! wins; outside git (a source tarball) it's "unknown".

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    // Rebuild when the checked-out commit changes.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    if let Ok(head) = std::fs::read_to_string("../../.git/HEAD")
        && let Some(r) = head.trim().strip_prefix("ref: ")
    {
        println!("cargo:rerun-if-changed=../../.git/{r}");
    }
    let commit = std::env::var("GITHUB_SHA")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let out = Command::new("git")
                .args(["rev-parse", "HEAD"])
                .output()
                .ok()?;
            out.status
                .success()
                .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        })
        .map(|s| s.chars().take(9).collect::<String>())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=UK_QOL_COMMIT={commit}");
}
