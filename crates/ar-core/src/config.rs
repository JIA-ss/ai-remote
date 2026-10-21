//! The host configuration file (`ai-remote.toml` in the host role directory).
//!
//! ```toml
//! [host]
//! name = "office-pc"
//! approval_timeout_secs = 60
//! approval_prompt = true # false: only approvers on paired devices can answer
//!
//! [network]
//! relays = []            # empty: the default public relays
//! relay_fallback_n0 = true # with relays set: keep n0's public relays as a fallback
//! proxy = ""             # empty: detect (HTTPS_PROXY, system setting); "none": never; or a URL
//! lan_discovery = "off"  # find peers on the local network: "on", "resolve" (do not announce), "off"
//! port_mapping = false   # ask the router to open a port (UPnP, NAT-PMP, PCP); the app's Network setting
//! port = 0               # a fixed UDP port to forward on the router; 0 or absent: random
//! mode = "auto"          # "lan": direct on the local network only, no relay, no address lookup
//! quiet = false          # true: local discovery and port mapping stay off whatever the lines above say
//! address_lookup = []    # empty: n0's address lookup service; or self-hosted pkarr server URLs
//!
//! [[rule]]               # evaluated in order; the first match wins
//! tool = "shell"
//! action = "ask"
//!
//! [[mount]]
//! name = "ui"
//! command = "C:/path/to/server.exe"
//! args = ["mcp"]
//! sha256 = "..."         # optional pin, checked before every start
//! start = "lazy"         # optional: spawn on first use (default "eager")
//! [mount.tools]
//! click = "control"
//! launch_app = { level = "control", approve = "ask" }
//! ```
//!
//! The `ai-remote mount ...` commands edit this file with [`add_mount`] / [`remove_mount`], which
//! keep the owner's comments and layout.

use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{Level, store};

/// Unknown keys are errors everywhere in this file: a misspelled key must stop the host with a
/// clear message, never be ignored (a misspelled `principal` would widen a rule to every device).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    #[serde(default)]
    pub host: HostSection,
    #[serde(default)]
    pub network: NetworkSection,
    /// Policy rules, evaluated in order before the built-in defaults.
    #[serde(default, rename = "rule", skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<PolicyRule>,
    /// Local MCP servers this host exposes to its controllers.
    #[serde(default, rename = "mount", skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<MountConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostSection {
    /// Display name shown to controllers; defaults to the machine name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// How long an approval prompt waits before it counts as "denied".
    #[serde(default = "default_approval_timeout")]
    pub approval_timeout_secs: u64,
    /// Ask on this computer's desktop as well as on approver devices. `false` leaves approvals to
    /// the devices whose approver keys this host accepted (for a machine nobody sits at).
    #[serde(default = "default_true")]
    pub approval_prompt: bool,
}

impl Default for HostSection {
    fn default() -> Self {
        Self { name: None, approval_timeout_secs: default_approval_timeout(), approval_prompt: true }
    }
}

fn default_true() -> bool {
    true
}

fn default_approval_timeout() -> u64 {
    60
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSection {
    /// Relay URLs to use instead of the default public relays.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relays: Vec<String>,
    /// With `relays` set, keep n0's public relays as a fallback (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_fallback_n0: Option<bool>,
    /// HTTP(S) proxy for relay connections: absent or empty detects it (environment, then the
    /// system setting), `"none"` never uses one, anything else is the proxy URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    /// Local-network (mDNS) discovery: `"on"`, `"resolve"` (find peers, do not announce) or `"off"`.
    /// Absent: `"off"`. `"on"` announces this device to every device on the network about once a
    /// second; it only helps where the devices have no internet at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lan_discovery: Option<String>,
    /// Ask the router to open a port to this device (UPnP, NAT-PMP, PCP), for direct connections
    /// behind NATs that hole punching alone does not get through. Absent: off. While there is no
    /// mapping the gateway is probed every 20 to 26 s. The desktop app's Network setting writes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port_mapping: Option<bool>,
    /// A fixed UDP port (IPv4) for someone who forwards it on their router; absent or 0: random. A
    /// taken port falls back to a random one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Quiet mode, for a managed network such as an office: local-network discovery and router port
    /// mapping stay off whatever `lan_discovery` and `port_mapping` say (the desktop app's Network
    /// setting cannot turn port mapping on). Both are off by default anyway; this locks them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet: Option<bool>,
    /// How this device reaches its peers: `"auto"` (absent: address lookup, hole punching and
    /// relays) or `"lan"` (direct on the local network only: no relay, no address lookup, nothing
    /// sent beyond the connections; a host listens on a fixed port, [`LAN_HOST_PORT`] unless `port`
    /// says otherwise, and a controller dials the address it stored for each host).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Self-hosted pkarr servers (iroh-dns-server) where devices publish and look up each other's
    /// addresses, instead of n0's service; empty: n0's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub address_lookup: Vec<String>,
}

/// The controller's configuration file (`ai-remote.toml` in the controller role directory): only
/// the network settings, which a missing file leaves at their defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControllerConfig {
    #[serde(default)]
    pub network: NetworkSection,
}

impl ControllerConfig {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("parse {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }
}

/// What the gate does with a matching call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    /// Run it.
    Allow,
    /// Ask the host owner first; deny if nobody answers in time or the desktop is locked.
    Ask,
    /// Refuse it.
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRule {
    /// Tool name, `*` wildcards allowed (`shell`, `ui_*`, `*`).
    pub tool: String,
    pub action: Action,
    /// Only for this caller (`eid:...`, `b64u:...` or a paired nickname).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
}

/// Unknown keys are errors: a misspelled `sha256`, `needs_desktop` or `approve` would otherwise
/// switch off a safeguard without a word.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MountConfig {
    /// Prefix for the mount's tools: `<name>_<tool>`.
    pub name: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Expected SHA-256 of `command`, checked before every start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// When the server process starts.
    #[serde(default, skip_serializing_if = "MountStart::is_default")]
    pub start: MountStart,
    /// Refuse calls while the desktop is locked or unusable (UI automation servers need it).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_desktop: bool,
    /// Allowlist: only these tools are exposed; anything else the server offers stays hidden.
    #[serde(default)]
    pub tools: BTreeMap<String, MountTool>,
}

/// When a mounted server's process runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MountStart {
    /// Started with the host and kept running (restarted on the next call after a crash).
    #[default]
    Eager,
    /// Started once at host start to read its tool list, then only on first use; stopped again
    /// after a while without calls.
    Lazy,
}

impl MountStart {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Either a bare level (`click = "control"`) or a table with more settings.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum MountTool {
    Level(Level),
    Detailed {
        level: Level,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        approve: Option<Action>,
        /// Replaces the server's own description (keeps the agent's context small).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
}

/// The table form of [`MountTool`], strict about its keys.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MountToolTable {
    level: Level,
    #[serde(default)]
    approve: Option<Action>,
    #[serde(default)]
    description: Option<String>,
}

// Written out rather than `#[serde(untagged)]`, whose error for a bad table is only "did not match
// any variant": this one names the unknown or missing key.
impl<'de> Deserialize<'de> for MountTool {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = MountTool;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str(r#"a level ("observe", "control", "admin") or a table such as { level = "control" }"#)
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<MountTool, E> {
                Level::deserialize(serde::de::value::StrDeserializer::new(v)).map(MountTool::Level)
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<MountTool, A::Error> {
                let t = MountToolTable::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                Ok(MountTool::Detailed { level: t.level, approve: t.approve, description: t.description })
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

impl MountTool {
    pub fn level(&self) -> Level {
        match self {
            MountTool::Level(l) => *l,
            MountTool::Detailed { level, .. } => *level,
        }
    }

    pub fn approve(&self) -> Option<Action> {
        match self {
            MountTool::Level(_) => None,
            MountTool::Detailed { approve, .. } => *approve,
        }
    }

    /// What the gate does when no policy rule matches: `approve` if set, otherwise the same default
    /// as built-in tools (admin tools ask, the rest run).
    pub fn action(&self) -> Action {
        self.approve().unwrap_or(if self.level() >= Level::Admin { Action::Ask } else { Action::Allow })
    }

    pub fn description(&self) -> Option<&str> {
        match self {
            MountTool::Level(_) => None,
            MountTool::Detailed { description, .. } => description.as_deref(),
        }
    }
}

impl HostConfig {
    /// Loads the file, or the defaults when it does not exist yet.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("parse {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        store::write_atomic(path, toml::to_string_pretty(self)?.as_bytes())
    }
}

/// Longest accepted mount name, so `<mount>_<tool>` keeps room for the tool part within the
/// 64-character tool-name limit of MCP clients.
pub const MOUNT_NAME_MAX: usize = 24;

/// Mount names become tool-name prefixes, so they use the tool-name alphabet.
pub fn validate_mount_name(name: &str) -> Result<()> {
    let ok_chars = name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if name.is_empty() || name.len() > MOUNT_NAME_MAX || !ok_chars {
        bail!(
            "invalid mount name '{name}': use 1-{MOUNT_NAME_MAX} characters from A-Z, a-z, 0-9, '_' and '-' \
             (it prefixes the mount's tool names, e.g. ui_click)"
        );
    }
    Ok(())
}

/// Appends a `[[mount]]` entry to the host config file, keeping everything else in the file
/// (comments included) as it was. Refuses a name that is already configured.
pub fn add_mount(path: &Path, mount: &MountConfig) -> Result<()> {
    validate_mount_name(&mount.name)?;
    let text = read_or_empty(path)?;
    let typed: HostConfig = toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    if typed.mounts.iter().any(|m| m.name == mount.name) {
        bail!("a mount named '{0}' already exists; remove it first (ai-remote mount remove {0})", mount.name);
    }
    let mut doc: toml_edit::DocumentMut = text.parse().with_context(|| format!("parse {}", path.display()))?;
    let table = mount_table(mount)?;
    match doc.get_mut("mount") {
        None => {
            let mut array = toml_edit::ArrayOfTables::new();
            array.push(table);
            doc.insert("mount", toml_edit::Item::ArrayOfTables(array));
        }
        Some(toml_edit::Item::ArrayOfTables(array)) => array.push(table),
        Some(_) => bail!("`mount` in {} is not a list of [[mount]] tables; fix the file by hand", path.display()),
    }
    store::write_atomic(path, doc.to_string().as_bytes())
}

/// Removes the `[[mount]]` entry called `name`. Returns false when there is none.
pub fn remove_mount(path: &Path, name: &str) -> Result<bool> {
    let text = read_or_empty(path)?;
    let mut doc: toml_edit::DocumentMut = text.parse().with_context(|| format!("parse {}", path.display()))?;
    let Some(array) = doc.get_mut("mount").and_then(toml_edit::Item::as_array_of_tables_mut) else {
        return Ok(false);
    };
    let Some(index) = array.iter().position(|t| t.get("name").and_then(|v| v.as_str()) == Some(name)) else {
        return Ok(false);
    };
    array.remove(index);
    if array.is_empty() {
        doc.remove("mount");
    }
    store::write_atomic(path, doc.to_string().as_bytes())?;
    Ok(true)
}

/// The UDP port a host listens on in LAN mode (`[network] mode = "lan"`) when `port` is not set:
/// controllers store the host's address, so it must not change between starts.
pub const LAN_HOST_PORT: u16 = 47800;

/// Host addresses as a person types them: `ip`, `ip:port` or `[ipv6]:port`, several separated by
/// commas or spaces. An address without a port gets [`LAN_HOST_PORT`].
pub fn parse_host_addresses(text: &str) -> Result<Vec<std::net::SocketAddr>> {
    let mut out = Vec::new();
    for part in text.split(|c: char| c == ',' || c.is_whitespace()).filter(|p| !p.is_empty()) {
        let addr = match part.parse::<std::net::SocketAddr>() {
            Ok(addr) => addr,
            Err(_) => match part.trim_start_matches('[').trim_end_matches(']').parse::<std::net::IpAddr>() {
                Ok(ip) => std::net::SocketAddr::new(ip, LAN_HOST_PORT),
                Err(_) => bail!("{part:?} is not an IP address (like 192.168.1.20 or 192.168.1.20:{LAN_HOST_PORT})"),
            },
        };
        out.push(addr);
    }
    if out.is_empty() {
        bail!("no address given");
    }
    Ok(out)
}

/// Sets `[network] port_mapping` in a host's or a controller's `ai-remote.toml`, keeping the rest of
/// the file (comments and layout included); creates the file when there is none.
pub fn set_port_mapping(path: &Path, on: bool) -> Result<()> {
    set_network_value(path, "port_mapping", on)
}

/// Refuses to turn local-network discovery or port mapping on when one of `paths` (a host's and a
/// controller's `ai-remote.toml`) says `quiet = true`: that lock is the point of quiet mode, so a
/// setting must not quietly write around it.
pub fn ensure_not_quiet(paths: &[&Path]) -> Result<()> {
    for path in paths {
        let text = read_or_empty(path)?;
        let doc: toml::Table = text.parse().with_context(|| format!("parse {}", path.display()))?;
        if doc.get("network").and_then(|n| n.get("quiet")).and_then(toml::Value::as_bool) == Some(true) {
            bail!(
                "quiet = true in {} keeps local-network discovery and port mapping off; remove that line to turn them on",
                path.display()
            );
        }
    }
    Ok(())
}

/// Sets `[network] lan_discovery` (`"on"`, `"resolve"` or `"off"`) the same way as
/// [`set_port_mapping`].
pub fn set_lan_discovery(path: &Path, value: &str) -> Result<()> {
    if !matches!(value, "on" | "resolve" | "off") {
        bail!("local-network discovery {value:?}: expected \"on\", \"resolve\" or \"off\"");
    }
    set_network_value(path, "lan_discovery", value)
}

/// Sets `[network] mode` (`"auto"` or `"lan"`) the same way as [`set_port_mapping`].
pub fn set_network_mode(path: &Path, mode: &str) -> Result<()> {
    if !matches!(mode, "auto" | "lan") {
        bail!("network mode {mode:?}: expected \"auto\" or \"lan\"");
    }
    set_network_value(path, "mode", mode)
}

fn set_network_value(path: &Path, key: &str, value: impl Into<toml_edit::Value>) -> Result<()> {
    let text = read_or_empty(path)?;
    let mut doc: toml_edit::DocumentMut = text.parse().with_context(|| format!("parse {}", path.display()))?;
    let network = doc.entry("network").or_insert(toml_edit::table());
    let Some(network) = network.as_table_mut() else {
        bail!("`network` in {} is not a [network] table; fix the file by hand", path.display());
    };
    network.insert(key, toml_edit::value(value));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    store::write_atomic(path, doc.to_string().as_bytes())
}

fn read_or_empty(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

/// One mount as a TOML table in the compact style of the documentation: one line per tool under
/// `[mount.tools]`.
fn mount_table(mount: &MountConfig) -> Result<toml_edit::Table> {
    let doc: toml_edit::DocumentMut = toml::to_string(mount)?.parse()?;
    let mut table = doc.as_table().clone();
    // A table parsed from another document carries that document's positions; unset them so the
    // new entry prints after the ones already in the file.
    table.set_position(None);
    if let Some(env) = table.get_mut("env").and_then(toml_edit::Item::as_table_mut) {
        env.set_position(None);
    }
    if let Some(tools) = table.get_mut("tools").and_then(toml_edit::Item::as_table_mut) {
        tools.set_position(None);
        let names: Vec<String> = tools.iter().map(|(k, _)| k.to_string()).collect();
        for name in names {
            inline_subtable(tools, &name);
        }
    }
    Ok(table)
}

/// Turns `table.key` from a `[section]` into `key = { ... }`.
fn inline_subtable(table: &mut toml_edit::Table, key: &str) {
    let Some(item) = table.get_mut(key) else { return };
    let Some(sub) = item.as_table() else { return };
    *item = toml_edit::value(sub.clone().into_inline_table());
    // The key was formatted as a section header; reset it to the `key = value` default spacing.
    if let Some(mut k) = table.key_mut(key) {
        k.leaf_decor_mut().clear();
    }
}

/// Matches `name` against a pattern where `*` matches any run of characters.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    fn rec(p: &[u8], n: &[u8]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some(b'*'), _) => rec(&p[1..], n) || (!n.is_empty() && rec(p, &n[1..])),
            (Some(a), Some(b)) if a == b => rec(&p[1..], &n[1..]),
            _ => false,
        }
    }
    rec(pattern.as_bytes(), name.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn misspelled_keys_are_errors() {
        // `principle` instead of `principal` must not silently turn into a rule for everyone.
        let typo = "[[rule]]
tool = \"shell\"
action = \"allow\"
principle = \"bob\"
";
        assert!(toml::from_str::<HostConfig>(typo).is_err());
        assert!(
            toml::from_str::<HostConfig>(
                "[[rules]]
tool = \"*\"
action = \"deny\"
"
            )
            .is_err()
        );
        assert!(
            toml::from_str::<HostConfig>(
                "[host]
nmae = \"x\"
"
            )
            .is_err()
        );
    }

    #[test]
    fn glob() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("ui_*", "ui_click"));
        assert!(!glob_match("ui_*", "shell"));
        assert!(glob_match("shell", "shell"));
        assert!(glob_match("*_window*", "focus_window"));
    }

    #[test]
    fn parses_example() {
        let cfg: HostConfig = toml::from_str(
            r#"
            [host]
            name = "office-pc"
            [[rule]]
            tool = "shell"
            action = "ask"
            [[mount]]
            name = "ui"
            command = "server.exe"
            args = ["mcp"]
            [mount.tools]
            click = "control"
            launch_app = { level = "control", approve = "ask" }
            "#,
        )
        .unwrap();
        assert_eq!(cfg.host.name.as_deref(), Some("office-pc"));
        assert_eq!(cfg.host.approval_timeout_secs, 60);
        assert_eq!(cfg.rules[0].action, Action::Ask);
        let tools = &cfg.mounts[0].tools;
        assert_eq!(tools["click"].level(), Level::Control);
        assert_eq!(tools["launch_app"].approve(), Some(Action::Ask));
        assert_eq!(cfg.mounts[0].start, MountStart::Eager);
        assert!(!cfg.mounts[0].needs_desktop);
    }

    #[test]
    fn misspelled_mount_keys_are_errors() {
        let base = "[[mount]]
name = \"ui\"
command = \"server.exe\"
";
        for (extra, key) in [
            (
                "sha265 = \"00\"
",
                "sha265",
            ),
            (
                "needs_dekstop = true
",
                "needs_dekstop",
            ),
            (
                "[mount.tools]
launch_app = { level = \"control\", aprove = \"ask\" }
",
                "aprove",
            ),
        ] {
            let err = toml::from_str::<HostConfig>(&format!("{base}{extra}")).unwrap_err().to_string();
            assert!(err.contains(key), "the error must name `{key}`: {err}");
        }
        let err = toml::from_str::<HostConfig>(&format!(
            "{base}[mount.tools]
click = \"contrl\"
"
        ))
        .unwrap_err()
        .to_string();
        assert!(err.contains("contrl"), "{err}");
        let err = toml::from_str::<HostConfig>(&format!(
            "{base}[mount.tools]
click = {{ approve = \"ask\" }}
"
        ))
        .unwrap_err()
        .to_string();
        assert!(err.contains("level"), "{err}");
    }

    #[test]
    fn mount_names() {
        assert!(validate_mount_name("ui").is_ok());
        assert!(validate_mount_name("cua-driver_2").is_ok());
        assert!(validate_mount_name("").is_err());
        assert!(validate_mount_name("ui.tools").is_err());
        assert!(validate_mount_name("a b").is_err());
        assert!(validate_mount_name(&"x".repeat(MOUNT_NAME_MAX + 1)).is_err());
    }

    fn sample_mount(name: &str) -> MountConfig {
        MountConfig {
            name: name.into(),
            command: "C:/tools/server.exe".into(),
            args: vec!["mcp".into()],
            env: BTreeMap::from([("TELEMETRY".into(), "false".into())]),
            sha256: Some("ab".repeat(32)),
            start: MountStart::Lazy,
            needs_desktop: true,
            tools: BTreeMap::from([
                ("click".into(), MountTool::Level(Level::Control)),
                (
                    "launch_app".into(),
                    MountTool::Detailed {
                        level: Level::Control,
                        approve: Some(Action::Ask),
                        description: Some("Start an app.".into()),
                    },
                ),
            ]),
        }
    }

    #[test]
    fn add_and_remove_mounts_keep_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ai-remote.toml");
        let original = "# owner's notes\n[host]\nname = \"office-pc\" # display name\n\n[[rule]]\ntool = \"shell\"\naction = \"ask\"\n";
        std::fs::write(&path, original).unwrap();

        add_mount(&path, &sample_mount("ui")).unwrap();
        add_mount(&path, &sample_mount("web")).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(original), "existing content must stay untouched:\n{text}");
        assert!(text.contains("launch_app = {"), "tool entries are inline tables:\n{text}");
        let cfg = HostConfig::load(&path).unwrap();
        assert_eq!(cfg.host.name.as_deref(), Some("office-pc"));
        assert_eq!(cfg.rules.len(), 1);
        assert_eq!(cfg.mounts.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["ui", "web"]);
        let ui = &cfg.mounts[0];
        assert_eq!(ui.start, MountStart::Lazy);
        assert!(ui.needs_desktop);
        assert_eq!(ui.env["TELEMETRY"], "false");
        assert_eq!(ui.tools["launch_app"].approve(), Some(Action::Ask));
        assert_eq!(ui.tools["launch_app"].description(), Some("Start an app."));
        assert_eq!(ui.tools["click"].level(), Level::Control);

        assert!(add_mount(&path, &sample_mount("ui")).is_err(), "duplicate names are refused");

        assert!(remove_mount(&path, "ui").unwrap());
        assert!(!remove_mount(&path, "ui").unwrap());
        let cfg = HostConfig::load(&path).unwrap();
        assert_eq!(cfg.mounts.len(), 1);
        assert_eq!(cfg.mounts[0].name, "web");
        assert!(remove_mount(&path, "web").unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("mount"), "no empty mount list left behind:\n{text}");
        assert!(text.contains("# owner's notes"));
    }

    #[test]
    fn add_mount_creates_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ai-remote.toml");
        add_mount(&path, &sample_mount("ui")).unwrap();
        assert_eq!(HostConfig::load(&path).unwrap().mounts[0].name, "ui");
    }

    #[test]
    fn parses_host_addresses_as_people_type_them() {
        let got = parse_host_addresses("192.168.1.20, 192.168.1.20:50000 [fe80::1]:9 ::1").unwrap();
        let want = ["192.168.1.20:47800", "192.168.1.20:50000", "[fe80::1]:9", "[::1]:47800"];
        assert_eq!(got.iter().map(ToString::to_string).collect::<Vec<_>>(), want);
        assert!(parse_host_addresses("win-dev").is_err(), "names are not resolved");
        assert!(parse_host_addresses(" , ").is_err());
    }

    #[test]
    fn set_port_mapping_keeps_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host").join("ai-remote.toml");
        set_port_mapping(&path, true).unwrap();
        assert_eq!(HostConfig::load(&path).unwrap().network.port_mapping, Some(true));

        let text = "# office\n[network]\nquiet = true # locked\n\n[host]\nname = \"pc\"\n";
        std::fs::write(&path, text).unwrap();
        set_port_mapping(&path, false).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("# office") && after.contains("quiet = true # locked"), "{after}");
        let cfg = HostConfig::load(&path).unwrap();
        assert_eq!((cfg.network.port_mapping, cfg.network.quiet), (Some(false), Some(true)));
        assert_eq!(cfg.host.name.as_deref(), Some("pc"));

        set_network_mode(&path, "lan").unwrap();
        let cfg = HostConfig::load(&path).unwrap();
        assert_eq!((cfg.network.mode.as_deref(), cfg.network.port_mapping), (Some("lan"), Some(false)));
        assert!(set_network_mode(&path, "offline").is_err(), "only auto or lan");
        assert!(ensure_not_quiet(&[&path]).is_err(), "this file says quiet = true");
        set_lan_discovery(&path, "on").unwrap();
        assert_eq!(HostConfig::load(&path).unwrap().network.lan_discovery.as_deref(), Some("on"));
        assert!(set_lan_discovery(&path, "yes").is_err());
    }
}
