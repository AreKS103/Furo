//! Project Furo — JSON Settings Store
//!
//! Thread-safe file-backed settings persisted to `%APPDATA%/Furo/settings.json`.

use parking_lot::Mutex;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

fn defaults() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("microphone".into(), String::new());
    m.insert(
        "model".into(),
        "deepdml/faster-whisper-large-v3-turbo-ct2".into(),
    );
    m.insert("hotkey_hold".into(), "ctrl+space".into());
    m.insert("hotkey_handsfree".into(), "ctrl+shift+space".into());
    m.insert("theme".into(), "dark".into());
    m.insert("language".into(), "en".into());
    m.insert("compute_type".into(), "int8_float16".into());
    m.insert("vad_threshold".into(), "0.45".into());
    m.insert("sound_enabled".into(), "true".into());
    m.insert("sound_volume".into(), "0.05".into());
    m
}

/// Thread-safe JSON settings with immediate persistence.
#[derive(Clone)]
pub struct SettingsStore {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    path: PathBuf,
    data: HashMap<String, String>,
}

impl SettingsStore {
    /// Create a new settings store. If `path` is `None`, uses
    /// `%APPDATA%/Furo/settings.json`.
    pub fn new(path: Option<PathBuf>) -> Self {
        let path = path.unwrap_or_else(|| {
            let config = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
            config.join("Furo").join("settings.json")
        });

        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let mut data = defaults();

        // Load saved settings from disk
        if path.exists() {
            match fs::read_to_string(&path) {
                Ok(contents) => {
                    if let Ok(saved) = serde_json::from_str::<HashMap<String, Value>>(&contents) {
                        for (k, v) in saved {
                            let s = match v {
                                Value::String(s) => s,
                                Value::Number(n) => n.to_string(),
                                Value::Bool(b) => b.to_string(),
                                _ => continue,
                            };
                            data.insert(k, s);
                        }
                    }
                    log::info!("Settings loaded from {}", path.display());
                }
                Err(e) => {
                    log::warn!("Could not load settings: {} — using defaults.", e);
                }
            }
        }

        // Migrate legacy "hotkey" → "hotkey_hold"
        if data.contains_key("hotkey") && !defaults().contains_key("hotkey") {
            if let Some(old_hk) = data.remove("hotkey") {
                if !data.contains_key("hotkey_hold")
                    || data.get("hotkey_hold") == defaults().get("hotkey_hold")
                {
                    log::info!("Migrated legacy hotkey '{}' → hotkey_hold.", old_hk);
                    data.insert("hotkey_hold".into(), old_hk);
                }
            }
        }

        // Migrate "hotkey_toggle" → "hotkey_hold"
        if let Some(old_hk) = data.remove("hotkey_toggle") {
            if !data.contains_key("hotkey_hold")
                || data.get("hotkey_hold") == defaults().get("hotkey_hold")
            {
                log::info!("Migrated hotkey_toggle '{}' → hotkey_hold.", old_hk);
                data.insert("hotkey_hold".into(), old_hk);
            }
        }

        let store = Self {
            inner: Arc::new(Mutex::new(Inner { path, data })),
        };
        store.save();
        store
    }

    pub fn get(&self, key: &str) -> String {
        let inner = self.inner.lock();
        inner
            .data
            .get(key)
            .cloned()
            .unwrap_or_else(|| defaults().get(key).cloned().unwrap_or_default())
    }

    pub fn set(&self, key: &str, value: &str) {
        let mut inner = self.inner.lock();
        inner.data.insert(key.into(), value.into());
        Self::save_inner(&inner);
    }

    /// Batch-update settings and persist. Returns the full settings map.
    pub fn update(&self, data: HashMap<String, String>) -> HashMap<String, String> {
        let mut inner = self.inner.lock();
        for (k, v) in data {
            inner.data.insert(k, v);
        }
        Self::save_inner(&inner);
        inner.data.clone()
    }

    pub fn all(&self) -> HashMap<String, String> {
        self.inner.lock().data.clone()
    }

    fn save(&self) {
        let inner = self.inner.lock();
        Self::save_inner(&inner);
    }

    fn save_inner(inner: &Inner) {
        match serde_json::to_string_pretty(&inner.data) {
            Ok(json) => {
                if let Err(e) = fs::write(&inner.path, json) {
                    log::error!("Could not save settings: {}", e);
                }
            }
            Err(e) => {
                log::error!("Could not serialize settings: {}", e);
            }
        }
    }
}
