//! Save sidecars (PLAN §2): `<save name>.qol.toml`, one table per mod, keyed
//! by hero. Interface fixed; implementation pending.

use std::collections::BTreeMap;

use crate::host::ModSaveState;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sidecar {
    pub mods: BTreeMap<String, ModSaveState>,
}

impl Sidecar {
    pub fn parse(text: &str) -> Result<Sidecar, String> {
        let _ = text;
        Ok(Sidecar::default())
    }

    pub fn to_toml(&self) -> String {
        String::new()
    }
}
