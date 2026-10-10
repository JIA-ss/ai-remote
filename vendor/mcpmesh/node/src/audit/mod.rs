//! Local, append-only JSONL audit log. One record per session open/close, per
//! proxied MCP request line (method + tool NAME + a blake3 hash of the arguments — NEVER the raw
//! arguments), per blob fetch, and per trust event. Best-effort: an audit-write failure is a
//! logged warning, never a blocked or failed session. Local-only: nothing here is ever transmitted;
//! `internal audit` reads these files directly with no daemon and no network.
pub mod log;
pub mod record;

// The writer types (`AuditLog`, `AuditSink`) plus the per-session proxied-line correlator
// (`RequestAuditor`) are re-exported from `log.rs`.
pub use log::{AuditLog, AuditSink, RequestAuditor};
pub use record::{AuditKind, AuditRecord, args_hash, now_ts};

use mcpmesh_local_api::AuditSummaryResult;
use std::path::{Path, PathBuf};

/// Is `name` a monthly audit file (`YYYY-MM.jsonl`)? Returns the `YYYY-MM` month key if so. The
/// rotation unit is the calendar month, so a file name IS its month.
fn month_of_filename(name: &str) -> Option<String> {
    let stem = name.strip_suffix(".jsonl")?;
    let bytes = stem.as_bytes();
    // Shape: DDDD-DD (4 digits, dash, 2 digits).
    if stem.len() == 7
        && bytes[4] == b'-'
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[5..].iter().all(u8::is_ascii_digit)
    {
        Some(stem.to_string())
    } else {
        None
    }
}

/// Enumerate the monthly files in `dir` as `(month, path, size_bytes)`, sorted ascending by month
/// (oldest first). A missing dir yields an empty list (no audit written yet), not an error.
pub fn list_month_files(dir: &Path) -> std::io::Result<Vec<(String, PathBuf, u64)>> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(month) = month_of_filename(&name) {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            out.push((month, entry.path(), size));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Parse one monthly file into records. An unparseable line is SKIPPED with a warning (a torn final
/// line from a crash, or a forward-compatible unknown field) rather than failing the whole read —
/// the log is diagnostic, not transactional.
pub fn read_records(path: &Path) -> std::io::Result<Vec<AuditRecord>> {
    let body = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in body.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<AuditRecord>(line) {
            Ok(rec) => out.push(rec),
            Err(e) => tracing::warn!(%e, "skipping unparseable audit line"),
        }
    }
    Ok(out)
}

/// Read every record across all monthly files in chronological order (oldest month first, in-file
/// order within a month).
pub fn read_all_records(dir: &Path) -> std::io::Result<Vec<AuditRecord>> {
    let mut out = Vec::new();
    for (_, path, _) in list_month_files(dir)? {
        out.extend(read_records(&path)?);
    }
    Ok(out)
}

/// Filter records by optional kind and optional peer (both AND-combined; `None` matches all).
pub fn filter_records<'a>(
    records: &'a [AuditRecord],
    kind: Option<AuditKind>,
    peer: Option<&str>,
) -> Vec<&'a AuditRecord> {
    records
        .iter()
        .filter(|r| kind.is_none_or(|k| r.kind == k))
        // `peer` matches the display name OR the stable principal (#57 gate): the one identity
        // the docs now say to key on must be selectable from `internal audit tail --peer`.
        .filter(|r| {
            peer.is_none_or(|p| r.peer.as_deref() == Some(p) || r.principal.as_deref() == Some(p))
        })
        .collect()
}

/// Aggregate audit records into per-peer / per-service SESSION counts. A "session" is a
/// `SessionOpen` record; every other kind (proxied requests, blob fetches, trust events) is ignored.
/// Deterministic: `per_peer` / `per_service` are sorted ascending by name (BTreeMap iteration). PURE
/// over an injected record slice — the reconciliation test below asserts this equals a direct count
/// over `read_all_records` (the same JSONL `internal audit` reads). LOCAL-only: the caller reads the
/// daemon's own `default_audit_dir()`; this fn never touches the network. Surface-clean: the
/// keys are the record's nicknames / service names, never endpoints/transport vocabulary.
pub fn summarize_sessions(records: &[AuditRecord]) -> AuditSummaryResult {
    use std::collections::BTreeMap;
    let mut per_peer: BTreeMap<String, u64> = BTreeMap::new();
    let mut per_service: BTreeMap<String, u64> = BTreeMap::new();
    let mut total_sessions: u64 = 0;
    for rec in records {
        if rec.kind != AuditKind::SessionOpen {
            continue;
        }
        total_sessions += 1;
        if let Some(peer) = &rec.peer {
            *per_peer.entry(peer.clone()).or_default() += 1;
        }
        if let Some(service) = &rec.service {
            *per_service.entry(service.clone()).or_default() += 1;
        }
    }
    AuditSummaryResult {
        per_peer: per_peer.into_iter().collect(),
        per_service: per_service.into_iter().collect(),
        total_sessions,
    }
}

/// Delete every monthly file STRICTLY older than `before` (a `YYYY-MM` string), returning the
/// deleted months. String comparison is correct for zero-padded `YYYY-MM`. The `before` month itself
/// is KEPT (delete-before-this, not delete-including). This is the rotation/prune of the monthly log.
pub fn prune_before(dir: &Path, before: &str) -> std::io::Result<Vec<String>> {
    let mut deleted = Vec::new();
    for (month, path, _) in list_month_files(dir)? {
        if month.as_str() < before {
            std::fs::remove_file(&path)?;
            deleted.push(month);
        }
    }
    Ok(deleted)
}

/// Is `s` a well-formed zero-padded `YYYY-MM` month key? The `audit_prune` verb validates with
/// this up front (#88): `prune_before`'s string comparison is CORRECT for well-formed keys and
/// silently matches nothing for garbage, so a typo'd month would otherwise report a clean
/// empty prune instead of an error.
pub fn valid_month_key(s: &str) -> bool {
    let bytes = s.as_bytes();
    s.len() == 7
        && bytes[4] == b'-'
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[5..].iter().all(u8::is_ascii_digit)
        && ("01"..="12").contains(&&s[5..])
}

/// One filtered, paged read over the monthly files (#88's `audit_list`). Month-range bounds are
/// applied to the FILE list (the rotation unit), so an out-of-range month is skipped without
/// parsing a line of it; `kind`/`peer` then filter records. `total` counts every match;
/// `records` pages by `offset`/`limit`. Chronological: oldest month first, in-file order within
/// a month (the append order).
///
/// STREAMS each file line by line rather than materializing it (#88 gate): month-file size is
/// driven by inbound peer traffic, so `read_records`'s whole-file `String` + full `Vec` made
/// peak memory on the owner's own query proportional to how chatty its peers are. Here peak
/// memory is one line + the (limit-bounded) page. `total` still requires scanning every
/// in-range line — a count needs a pass, not retention. Unparseable lines are skipped with a
/// warning, same contract as [`read_records`].
pub fn list_page(
    dir: &Path,
    since: Option<&str>,
    until: Option<&str>,
    kind: Option<AuditKind>,
    peer: Option<&str>,
    limit: usize,
    offset: usize,
) -> std::io::Result<mcpmesh_local_api::AuditListResult> {
    use std::io::BufRead;
    let mut total: u64 = 0;
    let mut records = Vec::new();
    let mut to_skip = offset;
    for (month, path, _) in list_month_files(dir)? {
        if since.is_some_and(|s| month.as_str() < s) || until.is_some_and(|u| month.as_str() > u) {
            continue;
        }
        for line in std::io::BufReader::new(std::fs::File::open(&path)?).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let rec = match serde_json::from_str::<AuditRecord>(&line) {
                Ok(rec) => rec,
                Err(e) => {
                    tracing::warn!(%e, "skipping unparseable audit line");
                    continue;
                }
            };
            if kind.is_some_and(|k| rec.kind != k) {
                continue;
            }
            if peer.is_some_and(|p| rec.peer.as_deref() != Some(p)) {
                continue;
            }
            total += 1;
            if to_skip > 0 {
                to_skip -= 1;
            } else if records.len() < limit {
                records.push(rec);
            }
        }
    }
    Ok(mcpmesh_local_api::AuditListResult { records, total })
}

/// The oldest month to KEEP under a retention of `retain_months`, given the current `YYYY-MM`
/// (#88): the current month counts as month 1, so `retain_months = 2` in `2026-07` keeps
/// `2026-06` and `2026-07` and returns `"2026-06"` — the `before` argument for
/// [`prune_before`]. `None` when `retain_months` is 0 (keep forever) or `current` is malformed.
/// Pure (year, month) arithmetic — no date crate, the repo idiom.
pub fn retention_cutoff(current: &str, retain_months: u32) -> Option<String> {
    if retain_months == 0 || !valid_month_key(current) {
        return None;
    }
    let year: i64 = current[..4].parse().ok()?;
    let month: i64 = current[5..7].parse().ok()?;
    // Zero-based total months, minus (N - 1) to land on the oldest KEPT month.
    let total = year * 12 + (month - 1) - i64::from(retain_months - 1);
    if total < 0 {
        return None; // a window reaching before year 0 keeps everything representable
    }
    Some(format!("{:04}-{:02}", total / 12, total % 12 + 1))
}

/// Parse a kind filter string from the porcelain (`--kind request`) into an [`AuditKind`].
pub fn parse_kind(s: &str) -> Option<AuditKind> {
    match s {
        "session_open" => Some(AuditKind::SessionOpen),
        "session_close" => Some(AuditKind::SessionClose),
        "request" => Some(AuditKind::Request),
        "blob_fetch" => Some(AuditKind::BlobFetch),
        "trust" => Some(AuditKind::Trust),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::record::AuditRecord;

    fn seed(dir: &std::path::Path) {
        // Two monthly files: 2026-06 (one trust record) and 2026-07 (two request records + one
        // session_open), so list/filter/prune have something to bite on.
        crate::audit::log::append_record(
            dir,
            &AuditRecord::trust(
                "2026-06-30T23:59:59.000Z".into(),
                "pair".into(),
                Some("bob".into()),
                None,
            ),
        )
        .unwrap();
        crate::audit::log::append_record(
            dir,
            &AuditRecord::session_open(
                "2026-07-01T00:00:00.000Z".into(),
                Some("bob".into()),
                "notes".into(),
                None,
            ),
        )
        .unwrap();
        crate::audit::log::append_record(
            dir,
            &AuditRecord::proxied_notification(
                "2026-07-01T00:00:01.000Z".into(),
                Some("bob".into()),
                "notes".into(),
                "tools/list".into(),
                None,
                "blake3:deadbeef".into(),
                None,
            ),
        )
        .unwrap();
        crate::audit::log::append_record(
            dir,
            &AuditRecord::proxied_notification(
                "2026-07-01T00:00:02.000Z".into(),
                Some("alice".into()),
                "notes".into(),
                "tools/call".into(),
                Some("read_file".into()),
                "blake3:cafe".into(),
                None,
            ),
        )
        .unwrap();
    }

    #[test]
    fn lists_monthly_files_sorted() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let months: Vec<String> = list_month_files(dir.path())
            .unwrap()
            .into_iter()
            .map(|(m, _, _)| m)
            .collect();
        assert_eq!(months, vec!["2026-06".to_string(), "2026-07".to_string()]);
    }

    #[test]
    fn reads_and_filters_by_kind_and_peer() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let all = read_all_records(dir.path()).unwrap();
        assert_eq!(all.len(), 4);
        // Filter to request records only.
        let reqs = filter_records(&all, Some(AuditKind::Request), None);
        assert_eq!(reqs.len(), 2);
        // Filter to peer "alice".
        let alice = filter_records(&all, None, Some("alice"));
        assert_eq!(alice.len(), 1);
        assert_eq!(alice[0].tool.as_deref(), Some("read_file"));
        // #57: the same flag matches the STABLE principal — the identity the docs say to key
        // on must be selectable, not just the collidable display name.
        let mut with_principal = AuditRecord::session_open(
            "2026-07-02T00:00:00.000Z".into(),
            Some("alice".into()),
            "notes".into(),
            Some("eid:a11ce".into()),
        );
        with_principal.principal = Some("eid:a11ce".into());
        let all2 = [&all[..], &[with_principal]].concat();
        let by_eid = filter_records(&all2, None, Some("eid:a11ce"));
        assert_eq!(
            by_eid.len(),
            1,
            "a principal string selects the record its display name would hide among collisions"
        );
    }

    #[test]
    fn prune_deletes_months_strictly_before_the_boundary() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let deleted = prune_before(dir.path(), "2026-07").unwrap();
        assert_eq!(deleted, vec!["2026-06".to_string()]);
        assert!(!dir.path().join("2026-06.jsonl").exists());
        assert!(
            dir.path().join("2026-07.jsonl").exists(),
            "the boundary month is kept"
        );
    }

    #[test]
    fn session_summary_reconciles_with_the_raw_audit_log() {
        // Seed a FIXED record set spanning two months: SessionOpen for bob/notes (x2), alice/notes
        // (x1), a peer-less SessionOpen for kb (x1), plus non-session records (a proxied request, a
        // trust event) that MUST NOT be counted as sessions.
        let dir = tempfile::tempdir().unwrap();
        crate::audit::log::append_record(
            dir.path(),
            &AuditRecord::session_open(
                "2026-06-30T10:00:00.000Z".into(),
                Some("bob".into()),
                "notes".into(),
                None,
            ),
        )
        .unwrap();
        crate::audit::log::append_record(
            dir.path(),
            &AuditRecord::session_open(
                "2026-07-01T10:00:00.000Z".into(),
                Some("bob".into()),
                "notes".into(),
                None,
            ),
        )
        .unwrap();
        crate::audit::log::append_record(
            dir.path(),
            &AuditRecord::session_open(
                "2026-07-01T11:00:00.000Z".into(),
                Some("alice".into()),
                "notes".into(),
                None,
            ),
        )
        .unwrap();
        crate::audit::log::append_record(
            dir.path(),
            &AuditRecord::session_open("2026-07-01T12:00:00.000Z".into(), None, "kb".into(), None),
        )
        .unwrap();
        crate::audit::log::append_record(
            dir.path(),
            &AuditRecord::proxied_notification(
                "2026-07-01T13:00:00.000Z".into(),
                Some("bob".into()),
                "notes".into(),
                "tools/list".into(),
                None,
                "blake3:x".into(),
                None,
            ),
        )
        .unwrap();
        crate::audit::log::append_record(
            dir.path(),
            &AuditRecord::trust(
                "2026-07-01T14:00:00.000Z".into(),
                "pair".into(),
                Some("carol".into()),
                None,
            ),
        )
        .unwrap();

        let all = read_all_records(dir.path()).unwrap();
        let summary = summarize_sessions(&all);

        // total = 4 SessionOpen records (the proxied request + the trust event are excluded).
        assert_eq!(summary.total_sessions, 4);

        // RECONCILIATION: each per-peer session count equals an INDEPENDENT direct count via
        // filter_records — the SAME read path `internal audit --kind session_open --peer <p>` uses.
        for (peer, count) in &summary.per_peer {
            let direct =
                filter_records(&all, Some(AuditKind::SessionOpen), Some(peer)).len() as u64;
            assert_eq!(
                *count, direct,
                "per-peer session count must reconcile with the raw log for {peer}"
            );
        }

        // Concrete numbers (sorted ascending by name): bob=2, alice=1 (the peer-less kb session is not
        // attributed to a peer, so it is absent from per_peer but present in total_sessions).
        assert_eq!(
            summary.per_peer,
            vec![("alice".to_string(), 1), ("bob".to_string(), 2)]
        );
        // per_service: notes=3 (bob x2 + alice x1), kb=1 — sorted ascending by name.
        assert_eq!(
            summary.per_service,
            vec![("kb".to_string(), 1), ("notes".to_string(), 3)]
        );
    }

    /// #88: the retention window's boundary arithmetic, exactly. The e2e boot test proves boot
    /// CALLS the prune; the year-2020 file it seeds is far outside any window, so an off-by-one
    /// here would pass it — the boundary itself is pinned only by these.
    #[test]
    fn retention_cutoff_counts_the_current_month_as_month_one() {
        // retain 2 in July keeps June+July → the oldest KEPT month (prune_before's arg) is June.
        assert_eq!(retention_cutoff("2026-07", 2).as_deref(), Some("2026-06"));
        // retain 1 keeps only the current month.
        assert_eq!(retention_cutoff("2026-07", 1).as_deref(), Some("2026-07"));
        // The window crosses a year boundary with zero-padded rendering.
        assert_eq!(retention_cutoff("2026-01", 3).as_deref(), Some("2025-11"));
        // 0 = keep forever; malformed input keeps everything rather than guessing.
        assert_eq!(retention_cutoff("2026-07", 0), None);
        assert_eq!(retention_cutoff("garbage", 2), None);
    }

    /// #88: the `audit_prune` input validation — well-formed months only, including the 01..=12
    /// month-number range (a "2026-13" would otherwise string-compare plausibly).
    #[test]
    fn month_key_validation_rejects_malformed_and_out_of_range() {
        assert!(valid_month_key("2026-07"));
        assert!(valid_month_key("1999-12"));
        assert!(valid_month_key("2026-01"));
        for bad in [
            "garbage",
            "2026-13",
            "2026-00",
            "2026-7",
            "202607",
            "2026-07-01",
            "",
        ] {
            assert!(!valid_month_key(bad), "must reject {bad:?}");
        }
    }
}
