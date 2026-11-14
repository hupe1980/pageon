//! Storage state in Playwright's `storageState` format: cookies, and `localStorage` per origin.
//! A person's logged-in browser state carries over to pageon, and back.

use serde::{Deserialize, Serialize};

use crate::{Cookie, Net, Storage};

/// Cookies and `localStorage`, as Playwright's `context.storageState()` writes them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StorageState {
    /// Cookies.
    #[serde(default)]
    pub cookies: Vec<Cookie>,
    /// `localStorage` by origin.
    #[serde(default)]
    pub origins: Vec<OriginState>,
}

/// One origin's `localStorage`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OriginState {
    /// `https://example.com`.
    pub origin: String,
    /// Items in insertion order.
    #[serde(default)]
    pub local_storage: Vec<Item>,
}

/// A storage item.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// Key.
    pub name: String,
    /// Value.
    pub value: String,
}

impl Net {
    /// The session's cookies (valid at `now`) and `localStorage`, origins sorted.
    pub fn storage_state(&self, now: u64) -> StorageState {
        let mut origins: Vec<OriginState> = self
            .local
            .iter()
            .filter(|(_, s)| !s.is_empty())
            .map(|(origin, s)| OriginState {
                origin: origin.clone(),
                local_storage: s
                    .keys()
                    .map(|k| Item {
                        name: k.to_string(),
                        value: s.get(k).unwrap_or_default().to_string(),
                    })
                    .collect(),
            })
            .collect();
        origins.sort_by(|a, b| a.origin.cmp(&b.origin));
        StorageState {
            cookies: self.cookies(now),
            origins,
        }
    }

    /// Adds the cookies and `localStorage` of a state (replacing equal cookies and keys).
    pub fn set_storage_state(&mut self, state: &StorageState) {
        self.add_cookies(&state.cookies);
        for o in &state.origins {
            let area: &mut Storage = self.local_storage(&o.origin);
            for item in &o.local_storage {
                let _ = area.set(&item.name, &item.value);
            }
        }
    }
}

impl StorageState {
    /// Reads a state: a Playwright `storageState` object, or a bare cookie array.
    pub fn from_json(text: &str) -> Result<Self, String> {
        from_json(text)
    }
}

/// Reads a state: a Playwright `storageState` object, or a bare cookie array.
pub fn from_json(text: &str) -> Result<StorageState, String> {
    if text.trim_start().starts_with('[') {
        return crate::cookie::from_json(text).map(|cookies| StorageState {
            cookies,
            origins: Vec::new(),
        });
    }
    serde_json::from_str(text).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playwright_state_round_trips() {
        let text = r#"{"cookies":[{"name":"sid","value":"1","domain":"a.test","path":"/","expires":-1,"httpOnly":true,"secure":true,"sameSite":"Lax"}],
            "origins":[{"origin":"https://a.test","localStorage":[{"name":"k","value":"v"}]}]}"#;
        let state = from_json(text).unwrap_or_else(|e| panic!("{e}"));
        let mut net = Net::default();
        net.set_storage_state(&state);
        let back = net.storage_state(0);
        assert_eq!(back.origins, state.origins);
        assert_eq!(back.cookies.len(), 1);
        let json = serde_json::to_string(&back).unwrap_or_default();
        assert!(
            json.contains("\"localStorage\":[{\"name\":\"k\",\"value\":\"v\"}]"),
            "{json}"
        );
        assert_eq!(from_json(&json).map(|s| s.cookies.len()), Ok(1));
    }
}
