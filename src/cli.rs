use std::{
    fs,
    io::{self, Write},
    path::Path,
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct DebugJson {
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub configurations: Vec<AdapterConfig>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AdapterConfig {
    pub label: String,
    pub adapter: String,
    pub request: String,
    pub program: String,
    pub cwd: String,
    #[serde(rename = "debugserverPort")]
    pub debugserver_port: u16,
}

fn default_version() -> String {
    "0.2.0".into()
}

impl Default for DebugJson {
    fn default() -> Self {
        Self {
            version: default_version(),
            configurations: Vec::new(),
        }
    }
}

pub fn load_debug_json(path: &Path) -> io::Result<DebugJson> {
    if !path.exists() {
        return Ok(DebugJson::default());
    }
    let contents = fs::read_to_string(path)?;
    let parsed = serde_json::from_str(&contents).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed to parse {}: {err}", path.display()),
        )
    })?;
    Ok(parsed)
}

pub fn save_debug_json(path: &Path, json: &DebugJson) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut file = fs::File::create(path)?;
    let body = serde_json::to_string_pretty(json).expect("serialize debug.json");
    file.write_all(body.as_bytes())?;
    Ok(())
}

pub fn upsert_configuration(configs: &mut Vec<AdapterConfig>, entry: AdapterConfig) {
    if let Some(existing) = configs.iter_mut().find(|cfg| cfg.label == entry.label) {
        *existing = entry;
    } else {
        configs.push(entry);
    }
}
