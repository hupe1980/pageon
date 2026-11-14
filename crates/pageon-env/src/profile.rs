//! Identity profiles: the honest `agent` default, or one pinned, coherent
//! Chrome chosen per session. Every layer reads its values from here: the network client, the
//! request headers, the client hints and what scripts see. Nothing is randomized.

use serde::{Deserialize, Serialize};

/// Who pageon says it is, at every layer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Profile {
    /// pageon, an automated agent: its own user agent, no client hints, `navigator.webdriver`
    /// true.
    #[default]
    Agent,
    /// Chrome 149 on Linux x86_64, coherent across TLS, HTTP/2, headers, client hints and the
    /// values scripts read (the network layer needs the `chrome` feature).
    BrowserCompatible,
}

/// The Chrome the browser-compatible profile pins (moving it is a bump of these and the
/// coherence test's values).
pub mod chrome {
    /// Major version.
    pub const MAJOR: u32 = 149;
    /// Full version (`navigator.userAgentData` high-entropy values).
    pub const FULL_VERSION: &str = "149.0.7795.2";
    /// The reduced user agent of Chrome on Linux.
    pub const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36";
    /// `navigator.platform`.
    pub const PLATFORM: &str = "Linux x86_64";
    /// `sec-ch-ua-platform` and `userAgentData.platform`.
    pub const UA_PLATFORM: &str = "Linux";
    /// `navigator.vendor`.
    pub const VENDOR: &str = "Google Inc.";
    /// Chrome's `accept` for documents.
    pub const ACCEPT_DOCUMENT: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7";
    /// Chrome's `accept-encoding`.
    pub const ACCEPT_ENCODING: &str = "gzip, deflate, br, zstd";
}

/// The `userAgentData` of a profile (UA Client Hints).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UaData {
    /// Brands with major versions, in `sec-ch-ua` order.
    pub brands: Vec<(String, String)>,
    /// Brands with full versions.
    pub full_version_list: Vec<(String, String)>,
    /// Mobile.
    pub mobile: bool,
    /// Platform (`Linux`).
    pub platform: &'static str,
    /// High-entropy values: architecture, bitness, model, platform version, full version.
    pub architecture: &'static str,
    /// `64`.
    pub bitness: &'static str,
    /// Empty on desktops.
    pub model: &'static str,
    /// The OS version.
    pub platform_version: &'static str,
    /// The full browser version.
    pub ua_full_version: &'static str,
    /// Form factors (`Desktop`).
    pub form_factors: Vec<&'static str>,
}

/// What `navigator` says under a profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Navigator {
    /// `navigator.userAgent`.
    pub user_agent: String,
    /// `navigator.platform`.
    pub platform: &'static str,
    /// `navigator.vendor`.
    pub vendor: &'static str,
    /// `navigator.webdriver`: whether an automation tool controls the user agent.
    pub webdriver: bool,
    /// `navigator.userAgentData` (absent for the agent profile).
    pub ua_data: Option<UaData>,
}

/// Chromium's GREASE brand for a major version, and the order of the three brands
/// (`components/embedder_support/user_agent_utils.cc`).
pub fn chrome_brands(major: u32, full: Option<&str>) -> Vec<(String, String)> {
    const CHARS: [&str; 11] = [" ", "(", ":", "-", ".", "/", ")", ";", "=", "?", "_"];
    const VERSIONS: [&str; 3] = ["8", "99", "24"];
    const ORDERS: [[usize; 3]; 6] = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    let s = major as usize;
    let grease = format!("Not{}A{}Brand", CHARS[s % 11], CHARS[(s + 1) % 11]);
    let gv = VERSIONS[s % 3];
    let v = full.map_or_else(|| major.to_string(), str::to_string);
    let input = [
        (
            grease,
            full.map_or_else(|| gv.to_string(), |_| format!("{gv}.0.0.0")),
        ),
        ("Chromium".to_string(), v.clone()),
        ("Google Chrome".to_string(), v),
    ];
    let mut out = vec![(String::new(), String::new()); 3];
    for (i, b) in input.into_iter().enumerate() {
        out[ORDERS[s % 6][i]] = b;
    }
    out
}

/// A brand list as a structured header (`"A";v="1", "B";v="2"`).
pub fn brand_header(brands: &[(String, String)]) -> String {
    brands
        .iter()
        .map(|(b, v)| format!("\"{b}\";v=\"{v}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

impl Profile {
    /// `agent` or `browser-compatible`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::BrowserCompatible => "browser-compatible",
        }
    }

    /// The `user-agent` header and `navigator.userAgent`.
    pub fn user_agent(self) -> String {
        match self {
            Self::Agent => crate::user_agent(),
            Self::BrowserCompatible => chrome::USER_AGENT.to_string(),
        }
    }

    /// The low-entropy client hints sent on requests to potentially trustworthy URLs.
    pub fn client_hints(self) -> Vec<(&'static str, String)> {
        match self {
            Self::Agent => Vec::new(),
            Self::BrowserCompatible => vec![
                (
                    "sec-ch-ua",
                    brand_header(&chrome_brands(chrome::MAJOR, None)),
                ),
                ("sec-ch-ua-mobile", "?0".into()),
                ("sec-ch-ua-platform", format!("\"{}\"", chrome::UA_PLATFORM)),
            ],
        }
    }

    /// What scripts read from `navigator`.
    pub fn navigator(self) -> Navigator {
        match self {
            Self::Agent => Navigator {
                user_agent: self.user_agent(),
                platform: "",
                vendor: "",
                webdriver: true,
                ua_data: None,
            },
            Self::BrowserCompatible => Navigator {
                user_agent: self.user_agent(),
                platform: chrome::PLATFORM,
                vendor: chrome::VENDOR,
                webdriver: false,
                ua_data: Some(UaData {
                    brands: chrome_brands(chrome::MAJOR, None),
                    full_version_list: chrome_brands(chrome::MAJOR, Some(chrome::FULL_VERSION)),
                    mobile: false,
                    platform: chrome::UA_PLATFORM,
                    architecture: "x86",
                    bitness: "64",
                    model: "",
                    platform_version: "6.8.0",
                    ua_full_version: chrome::FULL_VERSION,
                    form_factors: vec!["Desktop"],
                }),
            },
        }
    }
}

impl std::str::FromStr for Profile {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "agent" => Ok(Self::Agent),
            "browser-compatible" => Ok(Self::BrowserCompatible),
            other => Err(format!(
                "unknown profile {other:?}: use agent or browser-compatible"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grease_brands_follow_chromium() {
        // Observed on the wire: Chrome 157 and wreq-util's Chrome 149.
        assert_eq!(
            brand_header(&chrome_brands(157, None)),
            r#""Not-A.Brand";v="99", "Google Chrome";v="157", "Chromium";v="157""#
        );
        assert_eq!(
            brand_header(&chrome_brands(149, None)),
            r#""Google Chrome";v="149", "Chromium";v="149", "Not)A;Brand";v="24""#
        );
    }

    #[test]
    fn profiles_are_coherent() {
        let p = Profile::BrowserCompatible;
        let n = p.navigator();
        assert_eq!(n.user_agent, p.user_agent());
        let hints = p.client_hints();
        let ua = n.ua_data.unwrap();
        assert_eq!(hints[0].1, brand_header(&ua.brands));
        assert_eq!(hints[2].1, format!("\"{}\"", ua.platform));
        assert!(!n.webdriver);
        let a = Profile::Agent.navigator();
        assert!(a.webdriver && a.ua_data.is_none() && Profile::Agent.client_hints().is_empty());
        assert_eq!("browser-compatible".parse::<Profile>(), Ok(p));
    }
}
