//! Explicit domain selection shared by Manager and the independent sync process.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMode {
    #[default]
    Crypto,
    CnFutures,
}

pub fn read(path: &Path) -> Result<RuntimeMode> {
    #[derive(Deserialize)]
    struct Header {
        #[serde(default)]
        mode: RuntimeMode,
    }
    let text = std::fs::read_to_string(path).context("read runtime configuration")?;
    Ok(toml::from_str::<Header>(&text)
        .context("parse explicit runtime mode")?
        .mode)
}
