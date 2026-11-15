//! Identity profiles: the honest `agent` default, or one pinned, coherent
//! Chrome chosen per session. Every layer reads its values from here: the network client, the
//! request headers, the client hints and what scripts see. Nothing is randomized.

use serde::{Deserialize, Serialize};

/// Who pageon says it is, at every layer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Profile {
    /// pageon, an automated agent: its own user agent, no client hints, `navigator.webdriver`
    /// true.
    #[default]
    Agent,
    /// Chrome 149 on a desktop platform, coherent across TLS, HTTP/2, headers, client hints
    /// and the values scripts read (the network layer needs the `chrome` feature).
    BrowserCompatible(Platform),
}

/// The desktop platform the Chrome profile runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    /// Windows 11, x86-64.
    Windows,
    /// macOS, on the host's architecture.
    Macos,
    /// Linux, on the host's architecture.
    Linux,
}

impl Platform {
    /// The machine's own platform.
    pub fn host() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::Linux
        }
    }

    /// `windows`, `macos` or `linux`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Macos => "macos",
            Self::Linux => "linux",
        }
    }

    /// The user agent's platform token.
    fn ua_os(self) -> &'static str {
        match self {
            Self::Windows => "Windows NT 10.0; Win64; x64",
            // Chrome freezes the macOS token, on Apple silicon too.
            Self::Macos => "Macintosh; Intel Mac OS X 10_15_7",
            Self::Linux if arm() => "X11; Linux aarch64",
            Self::Linux => "X11; Linux x86_64",
        }
    }

    /// `navigator.platform`.
    pub fn navigator_platform(self) -> &'static str {
        match self {
            Self::Windows => "Win32",
            Self::Macos => "MacIntel",
            Self::Linux if arm() => "Linux aarch64",
            Self::Linux => "Linux x86_64",
        }
    }

    /// `sec-ch-ua-platform` and `userAgentData.platform`.
    pub fn ua_platform(self) -> &'static str {
        match self {
            Self::Windows => "Windows",
            Self::Macos => "macOS",
            Self::Linux => "Linux",
        }
    }

    /// `userAgentData`'s platform version.
    fn platform_version(self) -> &'static str {
        match self {
            // Windows 11 (as Chrome reports it).
            Self::Windows => "19.0.0",
            Self::Macos => "15.6.0",
            Self::Linux => "6.8.0",
        }
    }

    /// `userAgentData`'s architecture: the host's for macOS and Linux.
    fn architecture(self) -> &'static str {
        match self {
            Self::Windows => "x86",
            _ if arm() => "arm",
            _ => "x86",
        }
    }
}

fn arm() -> bool {
    cfg!(target_arch = "aarch64")
}

impl std::str::FromStr for Platform {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "host" => Ok(Self::host()),
            "windows" => Ok(Self::Windows),
            "macos" => Ok(Self::Macos),
            "linux" => Ok(Self::Linux),
            other => Err(format!(
                "unknown platform {other:?}: use host, windows, macos or linux"
            )),
        }
    }
}

/// The Chrome the browser-compatible profile pins (moving it is a bump of these and the
/// coherence test's values).
pub mod chrome {
    /// Major version.
    pub const MAJOR: u32 = 149;
    /// Full version (`navigator.userAgentData` high-entropy values).
    pub const FULL_VERSION: &str = "149.0.7795.2";
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
    /// `agent`, or `chrome:<platform>`.
    pub fn name(self) -> String {
        match self {
            Self::Agent => "agent".into(),
            Self::BrowserCompatible(p) => format!("chrome:{}", p.as_str()),
        }
    }

    /// The Chrome profile on the machine's own platform.
    pub fn chrome() -> Self {
        Self::BrowserCompatible(Platform::host())
    }

    /// The `user-agent` header and `navigator.userAgent`.
    pub fn user_agent(self) -> String {
        match self {
            Self::Agent => crate::user_agent(),
            Self::BrowserCompatible(p) => format!(
                "Mozilla/5.0 ({}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{}.0.0.0 Safari/537.36",
                p.ua_os(),
                chrome::MAJOR
            ),
        }
    }

    /// The low-entropy client hints sent on requests to potentially trustworthy URLs.
    pub fn client_hints(self) -> Vec<(&'static str, String)> {
        match self {
            Self::Agent => Vec::new(),
            Self::BrowserCompatible(p) => vec![
                (
                    "sec-ch-ua",
                    brand_header(&chrome_brands(chrome::MAJOR, None)),
                ),
                ("sec-ch-ua-mobile", "?0".into()),
                ("sec-ch-ua-platform", format!("\"{}\"", p.ua_platform())),
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
            Self::BrowserCompatible(p) => Navigator {
                user_agent: self.user_agent(),
                platform: p.navigator_platform(),
                vendor: chrome::VENDOR,
                webdriver: false,
                ua_data: Some(UaData {
                    brands: chrome_brands(chrome::MAJOR, None),
                    full_version_list: chrome_brands(chrome::MAJOR, Some(chrome::FULL_VERSION)),
                    mobile: false,
                    platform: p.ua_platform(),
                    architecture: p.architecture(),
                    bitness: "64",
                    model: "",
                    platform_version: p.platform_version(),
                    ua_full_version: chrome::FULL_VERSION,
                    form_factors: vec!["Desktop"],
                }),
            },
        }
    }
}

impl Serialize for Profile {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.name())
    }
}

impl<'de> Deserialize<'de> for Profile {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl std::str::FromStr for Profile {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.split_once(':') {
            None if s == "agent" => Ok(Self::Agent),
            None if s == "chrome" || s == "browser-compatible" => Ok(Self::chrome()),
            Some(("chrome" | "browser-compatible", p)) => Ok(Self::BrowserCompatible(p.parse()?)),
            _ => Err(format!("unknown profile {s:?}: use agent or chrome")),
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
        for (platform, os, nav, ch) in [
            (Platform::Windows, "Windows NT 10.0", "Win32", "Windows"),
            (Platform::Macos, "Mac OS X", "MacIntel", "macOS"),
            (Platform::Linux, "Linux", "Linux", "Linux"),
        ] {
            let p = Profile::BrowserCompatible(platform);
            let n = p.navigator();
            assert_eq!(n.user_agent, p.user_agent());
            assert!(n.user_agent.contains(os) && n.user_agent.contains("Chrome/149.0.0.0"));
            assert!(n.platform.starts_with(nav), "{}", n.platform);
            let hints = p.client_hints();
            let ua = n.ua_data.unwrap();
            assert_eq!(ua.platform, ch);
            assert_eq!(hints[0].1, brand_header(&ua.brands));
            assert_eq!(hints[2].1, format!("\"{ch}\""));
            assert!(!n.webdriver);
            assert_eq!(p.name().parse::<Profile>(), Ok(p));
        }
        let a = Profile::Agent.navigator();
        assert!(a.webdriver && a.ua_data.is_none() && Profile::Agent.client_hints().is_empty());
        assert_eq!(
            "browser-compatible".parse::<Profile>(),
            Ok(Profile::chrome())
        );
        assert_eq!("chrome".parse::<Profile>(), Ok(Profile::chrome()));
        assert!("chrome:beos".parse::<Profile>().is_err());
    }
}
