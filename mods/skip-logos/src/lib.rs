//! `skip-logos`: starts the game on the main menu without the startup logos
//! (README, "Mod designs"). Not implemented yet: the hook point is still being
//! reverse engineered (PLAN §5), so loading fails with a clear reason.

use uk_qol_runtime::host::{LoadCtx, Mod};

pub struct SkipLogos;

impl Mod for SkipLogos {
    fn id(&self) -> &'static str {
        "skip-logos"
    }

    fn manifest(&self) -> &'static str {
        include_str!("../mod.toml")
    }

    fn patterns(&self) -> &'static str {
        include_str!("../patterns.toml")
    }

    fn on_load(&mut self, _cx: &mut LoadCtx) -> Result<(), String> {
        Err("not implemented yet".into())
    }
}

/// Constructor passed to `uk_qol_runtime::start`.
pub fn new() -> Box<dyn Mod> {
    Box::new(SkipLogos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uk_qol_runtime::host::{Manifest, PatternFile};

    #[test]
    fn manifest_and_patterns_parse() {
        let m = new();
        let manifest = Manifest::parse(m.manifest()).unwrap();
        assert_eq!(manifest.id, m.id());
        assert!(manifest.settings.is_empty());
        assert!(
            PatternFile::parse(m.patterns())
                .unwrap()
                .patterns
                .is_empty()
        );
    }
}
