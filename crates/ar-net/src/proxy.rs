//! The HTTP(S) proxy relay connections go through, for networks that only let traffic out that way.
//!
//! Without it such a network loses the relay, and with it every peer that cannot be reached
//! directly. Only the relay's HTTPS/WebSocket connection uses the proxy; QUIC is UDP and never does.
//!
//! Order: the `[network] proxy` setting (a URL, or `"none"` to never use one), then the
//! `HTTPS_PROXY` / `ALL_PROXY` / `HTTP_PROXY` environment variables, then the system setting
//! (Windows: the user's Internet Settings; macOS: `scutil --proxy`). Proxy auto-config (PAC)
//! scripts are not evaluated.

/// What the `[network] proxy` setting asks for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ProxySetting {
    /// Use the environment or the system setting, if any.
    #[default]
    Detect,
    /// Never use a proxy.
    Off,
    /// Use this proxy.
    Url(String),
}

impl ProxySetting {
    /// Reads the config value: absent or empty detects, `none` / `off` / `direct` disables.
    pub fn parse(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            None | Some("") => Self::Detect,
            Some(v) if ["none", "off", "direct"].contains(&v.to_ascii_lowercase().as_str()) => Self::Off,
            Some(v) => Self::Url(v.to_string()),
        }
    }

    /// The proxy URL to hand the transport, and where it came from (for logs and doctor).
    pub fn resolve(&self) -> Option<(String, &'static str)> {
        match self {
            Self::Off => None,
            Self::Url(u) => normalize(u).map(|u| (u, "config")),
            Self::Detect => from_env(|k| std::env::var(k).ok())
                .map(|u| (u, "environment"))
                .or_else(|| system().map(|u| (u, "system"))),
        }
    }
}

/// `host:port` → `http://host:port`; keeps http(s) URLs; refuses other schemes (socks is not
/// supported by the transport).
fn normalize(value: &str) -> Option<String> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    let url = if v.contains("://") { v.to_string() } else { format!("http://{v}") };
    let scheme = url.split("://").next().unwrap_or_default().to_ascii_lowercase();
    (scheme == "http" || scheme == "https").then_some(url)
}

fn from_env(get: impl Fn(&str) -> Option<String>) -> Option<String> {
    ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy", "HTTP_PROXY", "http_proxy"]
        .iter()
        .find_map(|k| get(k).and_then(|v| normalize(&v)))
}

/// Windows `ProxyServer`: either `host:port` for every protocol, or `https=h:p;http=h:p;...`.
#[cfg_attr(not(windows), allow(dead_code))]
fn from_windows_server(server: &str) -> Option<String> {
    if !server.contains('=') {
        return normalize(server);
    }
    let pick = |want: &str| {
        server.split(';').find_map(|part| {
            let (k, v) = part.split_once('=')?;
            (k.trim().eq_ignore_ascii_case(want)).then(|| normalize(v)).flatten()
        })
    };
    pick("https").or_else(|| pick("http"))
}

/// `scutil --proxy` output: `HTTPSEnable : 1`, `HTTPSProxy : host`, `HTTPSPort : 8080` (and HTTP).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn from_scutil(output: &str) -> Option<String> {
    let get = |key: &str| {
        output.lines().find_map(|line| {
            let (k, v) = line.split_once(" : ")?;
            (k.trim() == key).then(|| v.trim().to_string())
        })
    };
    ["HTTPS", "HTTP"].iter().find_map(|p| {
        (get(&format!("{p}Enable")).as_deref() == Some("1")).then_some(())?;
        let host = get(&format!("{p}Proxy")).filter(|h| !h.is_empty())?;
        match get(&format!("{p}Port")) {
            Some(port) => normalize(&format!("{host}:{port}")),
            None => normalize(&host),
        }
    })
}

#[cfg(windows)]
fn system() -> Option<String> {
    let key =
        windows_registry::CURRENT_USER.open(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings").ok()?;
    if key.get_u32("ProxyEnable").ok()? == 0 {
        return None;
    }
    from_windows_server(&key.get_string("ProxyServer").ok()?)
}

#[cfg(target_os = "macos")]
fn system() -> Option<String> {
    let out = std::process::Command::new("/usr/sbin/scutil").arg("--proxy").output().ok()?;
    from_scutil(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn system() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_setting_detects_disables_or_names_a_proxy() {
        assert_eq!(ProxySetting::parse(None), ProxySetting::Detect);
        assert_eq!(ProxySetting::parse(Some(" ")), ProxySetting::Detect);
        assert_eq!(ProxySetting::parse(Some("None")), ProxySetting::Off);
        assert_eq!(ProxySetting::Off.resolve(), None);
        let set = ProxySetting::parse(Some("proxy.corp:3128"));
        assert_eq!(set.resolve(), Some(("http://proxy.corp:3128".into(), "config")));
        assert_eq!(ProxySetting::parse(Some("socks5://p:1080")).resolve(), None);
    }

    #[test]
    fn the_environment_prefers_https_then_all_then_http() {
        let env = |pairs: &[(&str, &str)]| {
            let pairs: Vec<(String, String)> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            from_env(move |k| pairs.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone()))
        };
        assert_eq!(env(&[]), None);
        assert_eq!(env(&[("http_proxy", "h:1"), ("HTTPS_PROXY", "https://s:2")]), Some("https://s:2".into()));
        assert_eq!(env(&[("http_proxy", "h:1"), ("all_proxy", "a:3")]), Some("http://a:3".into()));
        assert_eq!(env(&[("HTTPS_PROXY", "socks5://x:1"), ("HTTP_PROXY", "h:1")]), Some("http://h:1".into()));
    }

    #[test]
    fn reads_windows_proxy_server_values() {
        assert_eq!(from_windows_server("proxy:8080"), Some("http://proxy:8080".into()));
        assert_eq!(from_windows_server("http=h:80;https=s:443;ftp=f:21"), Some("http://s:443".into()));
        assert_eq!(from_windows_server("http=h:80"), Some("http://h:80".into()));
        assert_eq!(from_windows_server("socks=s:1080"), None);
    }

    #[test]
    fn reads_scutil_output() {
        let both = "<dictionary> {\n  HTTPEnable : 1\n  HTTPPort : 80\n  HTTPProxy : h.corp\n  HTTPSEnable : 1\n  HTTPSPort : 8443\n  HTTPSProxy : s.corp\n}\n";
        assert_eq!(from_scutil(both), Some("http://s.corp:8443".into()));
        let http_only =
            "<dictionary> {\n  HTTPEnable : 1\n  HTTPPort : 3128\n  HTTPProxy : h.corp\n  HTTPSEnable : 0\n}\n";
        assert_eq!(from_scutil(http_only), Some("http://h.corp:3128".into()));
        assert_eq!(from_scutil("<dictionary> {\n  ExceptionsList : <array> {\n  }\n}\n"), None);
    }
}
