//! The daemon's surgical `config.toml` read-modify-write writers. Every writer here follows
//! one discipline: [`read_config_for_rmw`] → parse as a `toml::Table` → mutate only the
//! touched keys → [`write_config_doc`] (render + the reference-doc header + an atomic
//! per-call-unique temp rename — no torn config). Comments/formatting are not preserved —
//! this is the machine-managed config surface; a `toml_edit` round-trip is a possible later
//! refinement.
//!
//! **Lock discipline (shared by every writer).** These are the LOCK-FREE halves of config
//! mutations: each caller holds `mesh.reload_lock` around its WHOLE critical section (read →
//! validate → write → reload → swap) and calls the writer DIRECTLY — a nested `reload_lock.lock()`
//! on the non-reentrant tokio Mutex would deadlock. The single-writer serialization lives at the
//! call sites (`install_roster` / `org_join` / `set_roster_url` / `register_service` /
//! `grant_service_access` / `revoke_service_access` / `rename_peer`), never here.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};
use mcpmesh_local_api::BackendSpec;

use crate::util::atomic_write;

/// Read a config file for a surgical read-modify-write, DISTINGUISHING a not-yet-created config
/// (legitimately empty — the first write) from a real IO error on an EXISTING file (e.g. its
/// permissions changed mid-life). `NotFound` → `""` (parses to an empty table, so the caller
/// edits from scratch); any other IO error propagates WITH context.
///
/// The bare `read_to_string(path).unwrap_or_default()` this replaces would coerce an unreadable
/// config to `""` → the strip/append silently finds nothing → a no-op returning `changed=false`,
/// i.e. a FALSE SUCCESS on an authorization mutation (a grant/revoke that never touched disk while
/// `pair`/`pair --remove` report success — the very orphan-allow the revoke-first ordering exists
/// to avoid). The allow-append/-remove callers must fail LOUDLY instead; the register path shares
/// the same class, so it uses this too.
pub(crate) fn read_config_for_rmw(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("read config {}", path.display())),
    }
}

/// The one-line comment header every managed write (re)adds to `config.toml`, pointing at the
/// key reference (issue #12). The TOML round-trip above drops comments, so the header is
/// prepended at write time by [`write_config_doc`] rather than stored — it therefore survives
/// every rewrite and never duplicates (the parse strips it, the write re-adds it).
const CONFIG_HEADER: &str = "# mcpmesh config — every table and key is documented in docs/config.md \
     (https://github.com/counterpunchtech/mcpmesh/blob/main/docs/config.md)";

/// Render a mutated config document and atomically replace the file, prepending
/// [`CONFIG_HEADER`] — the single write seam every writer in this module funnels through.
fn write_config_doc(path: &Path, doc: &toml::Table) -> Result<()> {
    let rendered = toml::to_string_pretty(doc).context("serialize config.toml")?;
    atomic_write(path, format!("{CONFIG_HEADER}\n{rendered}").as_bytes())
}

/// Surgically upsert string keys into ONE named top-level `[table]` of the config at `path`,
/// preserving every other key, then atomically replace the file — the shared body of the four
/// identity/roster pin writers below. Creates the table if absent; bails if the existing entry
/// under that name is not a table.
fn upsert_config_strings(path: &Path, table: &str, pairs: &[(&str, &str)]) -> Result<()> {
    let existing = read_config_for_rmw(path)?;
    let mut doc: toml::Table = toml::from_str(&existing)
        .with_context(|| format!("parse existing config {}", path.display()))?;
    let entry = doc
        .entry(table.to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let toml::Value::Table(entry) = entry else {
        anyhow::bail!("[{table}] in {} is not a table", path.display());
    };
    for (k, v) in pairs {
        entry.insert((*k).into(), toml::Value::String((*v).to_string()));
    }
    write_config_doc(path, &doc)
}

/// Upsert `[identity].org_root_pk` + `[identity].org_id` — the pin write `install_roster` runs
/// AFTER validation succeeds (pin-after-validate, see its doc).
pub(crate) fn write_identity_pin(path: &Path, org_root_pk: &str, org_id: &str) -> Result<()> {
    upsert_config_strings(
        path,
        "identity",
        &[("org_root_pk", org_root_pk), ("org_id", org_id)],
    )
}

/// Upsert `[identity].user_id` — the write `reconcile_user_id_from_roster` runs when the
/// installed roster's authoritative value differs from config's proposal.
pub(crate) fn write_identity_user_id(path: &Path, user_id: &str) -> Result<()> {
    upsert_config_strings(path, "identity", &[("user_id", user_id)])
}

/// Upsert the four `[identity]` join keys (the `org_join` control arm). `user_key` is a LOCAL
/// path string (the key file itself never crosses the API — only its path is recorded).
pub(crate) fn write_join_pin(
    path: &Path,
    org_id: &str,
    org_root_pk: &str,
    user_id: &str,
    user_key: &str,
) -> Result<()> {
    upsert_config_strings(
        path,
        "identity",
        &[
            ("org_id", org_id),
            ("org_root_pk", org_root_pk),
            ("user_id", user_id),
            ("user_key", user_key),
        ],
    )
}

/// Upsert `[roster].url` (the `set_roster_url` control arm). A pre-existing `grace_period` (or
/// any other `[roster]` key) is preserved.
pub(crate) fn write_roster_url(path: &Path, url: &str) -> Result<()> {
    upsert_config_strings(path, "roster", &[("url", url)])
}

/// Upsert `[identity].nickname` (the `set_nickname` control arm, #37). Every other
/// `[identity]` key (org pins, key paths, user_id) is preserved.
pub(crate) fn write_identity_nickname(path: &Path, nickname: &str) -> Result<()> {
    upsert_config_strings(path, "identity", &[("nickname", nickname)])
}

/// Set `[network].relay_mode = "custom"` + `[network].relay_urls = [urls…]` (the `set_relays`
/// control arm, #53). `relay_urls` is a TOML ARRAY, so this can't use [`upsert_config_strings`]
/// (string-keys only); it does the same surgical RMW by hand. Every other `[network]` key
/// (`discovery_mode`, `discovery_urls`) and every other table is preserved. The mode is forced
/// to `"custom"` so a later restart's `build_endpoint`/`net_plan` reproduces exactly this set.
pub(crate) fn write_relays(path: &Path, urls: &[String]) -> Result<()> {
    let existing = read_config_for_rmw(path)?;
    let mut doc: toml::Table = toml::from_str(&existing)
        .with_context(|| format!("parse existing config {}", path.display()))?;
    let entry = doc
        .entry("network".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let toml::Value::Table(network) = entry else {
        anyhow::bail!("[network] in {} is not a table", path.display());
    };
    network.insert("relay_mode".into(), toml::Value::String("custom".into()));
    network.insert(
        "relay_urls".into(),
        toml::Value::Array(
            urls.iter()
                .map(|u| toml::Value::String(u.clone()))
                .collect(),
        ),
    );
    write_config_doc(path, &doc)
}

/// Upsert `[services.<name>]` (atomic, surgical RMW), updating the backend while UNIONING the
/// incoming `allow` into any grants already on disk. Registration OWNS the backend; the allowlist
/// is co-owned by the pairing grant (`grant_service_access` appends nicknames). A re-registration
/// therefore must never DROP an existing allow entry — otherwise a service that re-registers on
/// every startup (kb does, always with an EMPTY allow) would silently revoke every paired peer.
/// So: keep the existing allow, append incoming names not already present (an explicit
/// `serve --allow bob` still adds a grant; removal is the separate `revoke_service_access` path).
/// Deliberately NOT routed through [`upsert_config_strings`] — this is a real merge, not a
/// string-key upsert.
/// Remove the WHOLE `[services.<name>]` entry (#50, the `unregister_service` verb). Idempotent:
/// an unknown name, or no `[services]` table, changes nothing (`Ok(false)`). The allow list goes
/// with it (grants are meaningless once the service is gone).
pub(crate) fn remove_service_from_config(path: &Path, name: &str) -> Result<bool> {
    let existing = read_config_for_rmw(path)?;
    let mut doc: toml::Table = toml::from_str(&existing)
        .with_context(|| format!("parse existing config {}", path.display()))?;
    let Some(toml::Value::Table(services)) = doc.get_mut("services") else {
        return Ok(false);
    };
    if services.remove(name).is_none() {
        return Ok(false); // unknown service → no-op
    }
    write_config_doc(path, &doc)?;
    Ok(true)
}

pub(crate) fn write_service_to_config(
    path: &Path,
    name: &str,
    backend: &BackendSpec,
    allow: &[String],
    // #63: `None` means "leave whatever is on disk alone", NOT "clear it". The entry table is
    // rebuilt from scratch below, so an unmerged field is silently erased — and every shipped
    // client sends `None`, which makes erasure the DEFAULT path rather than an edge case. `allow`
    // already had to learn this lesson; this is the same one.
    rate_limit_per_min: Option<u32>,
) -> Result<()> {
    let existing = read_config_for_rmw(path)?;
    let mut doc: toml::Table = toml::from_str(&existing)
        .with_context(|| format!("parse existing config {}", path.display()))?;

    let services = doc
        .entry("services".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let toml::Value::Table(services) = services else {
        anyhow::bail!("[services] in {} is not a table", path.display());
    };

    // Union the incoming allow with any grants already on disk (see the fn doc): a re-registration
    // updates the backend but must never silently drop a nickname a prior pairing appended.
    let mut merged_allow: Vec<String> = services
        .get(name)
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("allow"))
        .and_then(toml::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    for a in allow {
        if !merged_allow.iter().any(|existing| existing == a) {
            merged_allow.push(a.clone());
        }
    }

    let mut entry = toml::Table::new();
    match backend {
        BackendSpec::Run { cmd, env, cwd } => {
            entry.insert(
                "run".into(),
                toml::Value::Array(cmd.iter().cloned().map(toml::Value::String).collect()),
            );
            if let Some(cwd) = cwd {
                entry.insert("cwd".into(), toml::Value::String(cwd.clone()));
            }
            if !env.is_empty() {
                let env_tbl: toml::Table = env
                    .iter()
                    .map(|(k, v)| (k.clone(), toml::Value::String(v.clone())))
                    .collect();
                entry.insert("env".into(), toml::Value::Table(env_tbl));
            }
        }
        BackendSpec::Socket { path } => {
            entry.insert("socket".into(), toml::Value::String(path.clone()));
        }
    }
    entry.insert(
        "allow".into(),
        toml::Value::Array(merged_allow.into_iter().map(toml::Value::String).collect()),
    );
    // #63: carry the persisted rate across a re-registration. Supplied value wins; otherwise
    // whatever is already on disk survives.
    let preserved_rate = rate_limit_per_min.map(i64::from).or_else(|| {
        services
            .get(name)
            .and_then(toml::Value::as_table)
            .and_then(|t| t.get("rate_limit_per_min"))
            .and_then(toml::Value::as_integer)
    });
    if let Some(rate) = preserved_rate {
        entry.insert("rate_limit_per_min".into(), toml::Value::Integer(rate));
    }
    services.insert(name.to_string(), toml::Value::Table(entry));

    write_config_doc(path, &doc)
}

/// Append `principal` (a STABLE authz principal — `b64u:`/`eid:`/roster name, #38; never a
/// display nickname) to each `[services.<svc>].allow` in the config at `path`, idempotently,
/// and atomically rewrite the file. Returns whether the config actually CHANGED (so the caller
/// can skip a pointless reload).
///
/// A service NOT present in config is logged + skipped: a pairing grant authorizes into an
/// existing service, it never creates one. An already-present principal is a no-op for that
/// service (idempotent re-pair). Returns `Ok(false)` with no write when nothing changed.
///
/// `known_ephemeral` names services the caller has already resolved as ephemeral registrations
/// (#36). Their absence from config is the expected path, not a mistake, so they are logged at
/// `debug!` — #94 reported the `warn!` as a per-grant line "describing a condition that is correct
/// and expected". A name in NEITHER config nor this set still warns: that one is a real mistake
/// (a typo'd service in a pairing grant) and must stay visible.
pub(crate) fn append_allow_to_config(
    path: &Path,
    principal: &str,
    services: &[String],
    known_ephemeral: &HashSet<String>,
) -> Result<bool> {
    // Absent from config: expected for an ephemeral name, a mistake otherwise.
    let note_absent = |svc: &String| {
        if known_ephemeral.contains(svc) {
            tracing::debug!(service = %svc, "grant: ephemeral service, no config allow to append");
        } else {
            tracing::warn!(service = %svc, "grant: service not in config; skipping allow-append");
        }
    };
    let existing = read_config_for_rmw(path)?;
    let mut doc: toml::Table = toml::from_str(&existing)
        .with_context(|| format!("parse existing config {}", path.display()))?;

    // No `[services]` table at all → nothing to grant into (log each, change nothing).
    let Some(toml::Value::Table(services_tbl)) = doc.get_mut("services") else {
        services.iter().for_each(note_absent);
        return Ok(false);
    };

    let mut changed = false;
    for svc in services {
        let Some(entry) = services_tbl.get_mut(svc) else {
            note_absent(svc);
            continue;
        };
        let toml::Value::Table(entry) = entry else {
            anyhow::bail!("[services.{svc}] in {} is not a table", path.display());
        };
        let allow = entry
            .entry("allow".to_string())
            .or_insert_with(|| toml::Value::Array(Vec::new()));
        let toml::Value::Array(allow_arr) = allow else {
            anyhow::bail!(
                "[services.{svc}].allow in {} is not an array",
                path.display()
            );
        };
        // Idempotent: append only if not already granted.
        if allow_arr.iter().any(|v| v.as_str() == Some(principal)) {
            continue;
        }
        allow_arr.push(toml::Value::String(principal.to_string()));
        changed = true;
    }

    if changed {
        write_config_doc(path, &doc)?;
    }
    Ok(changed)
}

/// Remove every entry in `principals` from EVERY `[services.<svc>].allow` in ONE atomic
/// read-modify-write. `principals` are a peer's STABLE authz principals (#38: its device
/// `eid:`s plus, when unshared, its `b64u:` user_id) — never display nicknames, and never
/// bare roster names (a bare strip could hit a group). Returns whether the config changed.
///
/// **Fail-safe leniency (vs. the grant's strictness).** A malformed service entry (a non-table
/// `[services.<svc>]`, or a non-array `allow`) is SKIPPED, not an error: this is a removal that
/// only ever makes access MORE restricted, and its caller (`remove_peer`) aborts the whole
/// unpair on any error — so bailing on one weird entry would leave the peer LESS restricted (the
/// opposite of fail-safe). A genuinely unparseable config still errors (exceptional corruption,
/// same as the grant path). No `[services]` table → nothing to revoke (`Ok(false)`).
/// Remove a SINGLE `principal` from a SINGLE service's `[services.<service>].allow` (#44,
/// the `service_allow_revoke` verb). Idempotent: an absent principal, an unknown service, or a
/// malformed/absent allow all change nothing (`Ok(false)`) — never an error. Distinct from
/// [`remove_allow_from_config`] (which strips a peer's principals from EVERY service on unpair).
pub(crate) fn remove_principal_from_service(
    path: &Path,
    service: &str,
    principal: &str,
) -> Result<bool> {
    let existing = read_config_for_rmw(path)?;
    let mut doc: toml::Table = toml::from_str(&existing)
        .with_context(|| format!("parse existing config {}", path.display()))?;
    let Some(toml::Value::Table(services_tbl)) = doc.get_mut("services") else {
        return Ok(false);
    };
    let Some(toml::Value::Table(svc)) = services_tbl.get_mut(service) else {
        return Ok(false); // unknown service → nothing to revoke
    };
    let Some(toml::Value::Array(allow)) = svc.get_mut("allow") else {
        return Ok(false); // absent/malformed allow → nothing to revoke
    };
    let before = allow.len();
    allow.retain(|v| v.as_str().is_none_or(|s| s != principal));
    if allow.len() == before {
        return Ok(false); // principal not present → no-op
    }
    write_config_doc(path, &doc)?;
    Ok(true)
}

pub(crate) fn remove_allow_from_config(path: &Path, principals: &[String]) -> Result<bool> {
    let existing = read_config_for_rmw(path)?;
    let mut doc: toml::Table = toml::from_str(&existing)
        .with_context(|| format!("parse existing config {}", path.display()))?;

    let Some(toml::Value::Table(services_tbl)) = doc.get_mut("services") else {
        return Ok(false); // no [services] table → nothing to revoke
    };

    let mut changed = false;
    for (_, entry) in services_tbl.iter_mut() {
        let toml::Value::Table(svc) = entry else {
            continue; // malformed service entry → skip (fail-safe)
        };
        let Some(toml::Value::Array(allow)) = svc.get_mut("allow") else {
            continue; // absent or malformed allow → skip (fail-safe)
        };
        let before = allow.len();
        allow.retain(|v| {
            v.as_str()
                .is_none_or(|s| !principals.iter().any(|p| p == s))
        });
        if allow.len() != before {
            changed = true;
        }
    }
    if changed {
        write_config_doc(path, &doc)?;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    /// Every managed write prepends the one-line reference header pointing at docs/config.md
    /// (issue #12), a later rewrite does not duplicate it (the parse strips comments, the write
    /// re-adds one), and the headered file still loads.
    #[test]
    fn managed_writes_carry_the_reference_header_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_service_to_config(
            &path,
            "kb",
            &BackendSpec::Socket {
                path: "/run/kb.sock".into(),
            },
            &[],
            None,
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("# mcpmesh config") && text.contains("docs/config.md"),
            "the generated config opens with the reference header: {text}"
        );
        // A second managed write re-adds the header exactly once, and the file still parses.
        assert!(
            append_allow_to_config(&path, "bob", &["kb".to_string()], &HashSet::new()).unwrap()
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text.matches("# mcpmesh config").count(),
            1,
            "no duplicated header after a rewrite: {text}"
        );
        assert!(Config::load(&path).is_ok());
    }

    /// `write_join_pin` upserts the FOUR `[identity]` keys while preserving every other config key
    /// (surgical RMW) and lands atomically. Sync (no endpoint) — isolates the write discipline.
    #[test]
    fn write_join_pin_is_surgical() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[network]\nrelay_mode = \"disabled\"\n\n[identity]\nnickname = \"mydev\"\n\n\
             [services.notes]\nrun = [\"notes-mcp\"]\nallow = [\"alice\"]\n",
        )
        .unwrap();

        write_join_pin(
            &path,
            "acme",
            "b64u:ANCHOR",
            "alice",
            "/home/alice/user.key",
        )
        .unwrap();

        let doc: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let identity = doc["identity"].as_table().unwrap();
        assert_eq!(identity["org_id"].as_str(), Some("acme"));
        assert_eq!(identity["org_root_pk"].as_str(), Some("b64u:ANCHOR"));
        assert_eq!(identity["user_id"].as_str(), Some("alice"));
        assert_eq!(identity["user_key"].as_str(), Some("/home/alice/user.key"));
        // Every pre-existing key is preserved (surgical): nickname, the network table, the service.
        assert_eq!(identity["nickname"].as_str(), Some("mydev"));
        assert_eq!(
            doc["network"]["relay_mode"].as_str(),
            Some("disabled"),
            "unrelated [network] must be preserved"
        );
        assert!(
            doc["services"]["notes"].is_table(),
            "a pre-existing [services.*] must be preserved: {doc:?}"
        );
    }

    /// `write_roster_url` upserts `[roster].url`, creating the table when absent and preserving
    /// every other key (surgical RMW) — the `[roster]` sibling of `write_join_pin`.
    #[test]
    fn write_roster_url_upserts_and_preserves() {
        let dir = tempfile::tempdir().unwrap();
        // Pre-existing [roster] with another key → the key survives, url is added.
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[network]\nrelay_mode = \"disabled\"\n\n[roster]\ngrace_period = \"7d\"\n",
        )
        .unwrap();
        write_roster_url(&path, "https://acme.example/roster.json").unwrap();
        let doc: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            doc["roster"]["url"].as_str(),
            Some("https://acme.example/roster.json")
        );
        assert_eq!(doc["roster"]["grace_period"].as_str(), Some("7d"));
        assert_eq!(doc["network"]["relay_mode"].as_str(), Some("disabled"));
        // No [roster] table yet → it is created from scratch, other tables untouched.
        let path2 = dir.path().join("c2.toml");
        std::fs::write(&path2, "[identity]\nnickname = \"x\"\n").unwrap();
        write_roster_url(&path2, "https://h/r").unwrap();
        let doc2: toml::Table = toml::from_str(&std::fs::read_to_string(&path2).unwrap()).unwrap();
        assert_eq!(doc2["roster"]["url"].as_str(), Some("https://h/r"));
        assert_eq!(doc2["identity"]["nickname"].as_str(), Some("x"));
    }

    /// `write_identity_user_id` surgically upserts `[identity].user_id`, preserving other keys.
    #[test]
    fn write_identity_user_id_is_surgical() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[identity]\nnickname = \"dev\"\norg_id = \"acme\"\n").unwrap();
        write_identity_user_id(&path, "b64u:USER").unwrap();
        let doc: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc["identity"]["user_id"].as_str(), Some("b64u:USER"));
        assert_eq!(doc["identity"]["nickname"].as_str(), Some("dev"));
        assert_eq!(doc["identity"]["org_id"].as_str(), Some("acme"));
    }

    /// The shared upsert bails LOUDLY when the named entry exists but is NOT a table (a corrupt
    /// config must never be silently rewritten around).
    #[test]
    fn upsert_bails_on_a_non_table_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "identity = \"nope\"\n").unwrap();
        let err = write_identity_user_id(&path, "x").unwrap_err();
        assert!(
            format!("{err:#}").contains("[identity]"),
            "bail names the offending table: {err:#}"
        );
        // The file is untouched.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "identity = \"nope\"\n"
        );
    }

    /// `append_allow_to_config` / `remove_allow_from_config` grant + revoke a nickname in a service's
    /// `allow`, are idempotent, and return `false` (no write) when there is nothing to change.
    #[test]
    fn allow_config_append_and_remove_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[services.kb]\nsocket = \"/run/kb.sock\"\n").unwrap();

        // Grant bob → true, and bob is in the allow list.
        assert!(
            append_allow_to_config(&path, "bob", &["kb".to_string()], &HashSet::new()).unwrap()
        );
        let doc: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let allow = doc["services"]["kb"]["allow"].as_array().unwrap();
        assert!(allow.iter().any(|v| v.as_str() == Some("bob")));
        // Idempotent: re-granting bob is a no-op (false).
        assert!(
            !append_allow_to_config(&path, "bob", &["kb".to_string()], &HashSet::new()).unwrap()
        );
        // Granting into a service NOT in config → skipped, no change.
        assert!(
            !append_allow_to_config(&path, "carol", &["ghost".to_string()], &HashSet::new())
                .unwrap()
        );

        // Revoke bob's principals → true, allow becomes empty. (Principals-slice form, #38:
        // one atomic RMW strips every listed principal.)
        assert!(remove_allow_from_config(&path, &["bob".to_string()]).unwrap());
        let doc: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            doc["services"]["kb"]["allow"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        // Revoking absent principals → no change (false).
        assert!(!remove_allow_from_config(&path, &["nobody".to_string()]).unwrap());

        // No [services] table at all → both grant + revoke are false (nothing to touch).
        let empty = dir.path().join("empty.toml");
        std::fs::write(&empty, "[identity]\nnickname = \"x\"\n").unwrap();
        assert!(
            !append_allow_to_config(&empty, "bob", &["kb".to_string()], &HashSet::new()).unwrap()
        );
        assert!(!remove_allow_from_config(&empty, &["bob".to_string()]).unwrap());
    }

    /// Re-registering an existing service (kb does this idempotently on EVERY startup, with an
    /// EMPTY allow) must PRESERVE the grants a prior pairing appended to its `allow`. Otherwise a
    /// kb daemon restart silently revokes every paired peer's access to kb — the bug the Jetson
    /// P2P proof hit (a paired peer then gets `-32054 unknown or unauthorized service`).
    /// #63: a re-registration must PRESERVE a persisted `rate_limit_per_min`.
    ///
    /// The entry table is rebuilt from scratch, so an unmerged field is silently erased — and every
    /// shipped client sends `None`, which makes erasure the DEFAULT path rather than an edge case.
    /// `allow` already had to learn this; the first attempt at #63 shipped the same bug for the
    /// rate and the review caught it.
    #[test]
    fn reregistering_a_service_preserves_a_persisted_rate_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let socket = BackendSpec::Socket {
            path: "/run/kb.sock".into(),
        };

        // Registered WITH a rate.
        write_service_to_config(&path, "kb", &socket, &[], Some(7)).unwrap();
        assert_eq!(
            Config::load(&path)
                .unwrap()
                .services
                .get("kb")
                .unwrap()
                .rate_limit_per_min,
            Some(7)
        );

        // The daemon restarts and re-registers idempotently — with `None`, like every client does.
        write_service_to_config(&path, "kb", &socket, &[], None).unwrap();
        assert_eq!(
            Config::load(&path)
                .unwrap()
                .services
                .get("kb")
                .unwrap()
                .rate_limit_per_min,
            Some(7),
            "a re-registration that says nothing about the rate must not ERASE it — `None` means \
             'leave it alone', not 'clear it'"
        );

        // An explicit new value still wins.
        write_service_to_config(&path, "kb", &socket, &[], Some(3)).unwrap();
        assert_eq!(
            Config::load(&path)
                .unwrap()
                .services
                .get("kb")
                .unwrap()
                .rate_limit_per_min,
            Some(3),
            "a supplied value must override the persisted one"
        );
    }

    #[test]
    fn reregistering_a_service_preserves_existing_allow_grants() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let socket = BackendSpec::Socket {
            path: "/run/kb.sock".into(),
        };

        // 1. kb registers itself with an empty allow (reachability is a separate user grant).
        write_service_to_config(&path, "kb", &socket, &[], None).unwrap();
        // 2. Pairing grants "alice" access (appends to [services.kb].allow).
        append_allow_to_config(&path, "alice", &["kb".to_string()], &HashSet::new()).unwrap();
        // 3. The kb daemon RESTARTS → re-registers idempotently, again with an empty allow.
        write_service_to_config(&path, "kb", &socket, &[], None).unwrap();

        // The grant must survive the re-registration.
        let cfg = Config::load(&path).unwrap();
        assert_eq!(
            cfg.services.get("kb").unwrap().allow,
            vec!["alice".to_string()],
            "a kb re-registration must not wipe the pairing grant"
        );
    }

    /// A re-registration still UPDATES the backend and UNIONS any incoming allow (so an explicit
    /// `serve --allow bob` adds a grant) — it only refuses to silently DROP existing grants.
    #[test]
    fn reregistering_updates_backend_and_unions_incoming_allow() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // Initial: a socket backend allowing "alice".
        write_service_to_config(
            &path,
            "svc",
            &BackendSpec::Socket {
                path: "/a.sock".into(),
            },
            &["alice".to_string()],
            None,
        )
        .unwrap();
        // Re-register: a new socket path + a new allow entry "bob".
        write_service_to_config(
            &path,
            "svc",
            &BackendSpec::Socket {
                path: "/b.sock".into(),
            },
            &["bob".to_string()],
            None,
        )
        .unwrap();

        let cfg = Config::load(&path).unwrap();
        let svc = cfg.services.get("svc").unwrap();
        assert_eq!(svc.socket.as_deref(), Some("/b.sock"), "backend updates");
        assert_eq!(
            svc.allow,
            vec!["alice".to_string(), "bob".to_string()],
            "union: existing grant kept, incoming appended"
        );
    }

    /// #51: a Run backend's `env` + `cwd` round-trip through the config write + load.
    #[test]
    fn run_backend_env_and_cwd_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").unwrap();
        let mut env = std::collections::BTreeMap::new();
        env.insert("GITHUB_TOKEN".to_string(), "ghp_x".to_string());
        let spec = mcpmesh_local_api::BackendSpec::Run {
            cmd: vec!["npx".into(), "server".into()],
            env,
            cwd: Some("/home/me/code".into()),
        };
        write_service_to_config(&path, "gh", &spec, &["eid:beef".to_string()], None).unwrap();
        let cfg = Config::load(&path).unwrap();
        let svc = cfg.services.get("gh").unwrap();
        assert_eq!(svc.cwd.as_deref(), Some("/home/me/code"));
        assert_eq!(
            svc.env.get("GITHUB_TOKEN").map(String::as_str),
            Some("ghp_x")
        );
        assert_eq!(
            svc.run.as_deref().unwrap(),
            &["npx".to_string(), "server".to_string()]
        );
    }

    /// `write_relays` (#53) forces `relay_mode = "custom"`, writes the URL ARRAY, and preserves
    /// every other `[network]` key (`discovery_*`) plus other tables. Round-trips via `Config::load`
    /// and overwrites a prior `relay_urls` on a second call.
    #[test]
    fn write_relays_sets_custom_mode_and_preserves_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[network]\nrelay_mode = \"default\"\ndiscovery_mode = \"custom\"\n\
             discovery_urls = [\"https://dns.acme.com\"]\n[identity]\nnickname = \"dev\"\n",
        )
        .unwrap();

        write_relays(
            &path,
            &[
                "https://relay1.acme.com".into(),
                "https://relay2.acme.com".into(),
            ],
        )
        .unwrap();

        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.network.relay_mode, "custom");
        assert_eq!(
            cfg.network.relay_urls,
            vec![
                "https://relay1.acme.com".to_string(),
                "https://relay2.acme.com".to_string()
            ]
        );
        // Other [network] keys are preserved.
        assert_eq!(cfg.network.discovery_mode, "custom");
        assert_eq!(
            cfg.network.discovery_urls,
            vec!["https://dns.acme.com".to_string()]
        );
        // Other tables are preserved.
        let doc: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc["identity"]["nickname"].as_str(), Some("dev"));

        // A second call overwrites the array wholesale (not append).
        write_relays(&path, &["https://relay3.acme.com".into()]).unwrap();
        let cfg = Config::load(&path).unwrap();
        assert_eq!(
            cfg.network.relay_urls,
            vec!["https://relay3.acme.com".to_string()]
        );
    }
}
