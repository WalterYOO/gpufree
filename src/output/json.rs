//! JSON rendering. The schema (`gpufree/v1`) lives in `model`, so this is
//! only serialization; key order follows the struct declarations.

use crate::model::Snapshot;

pub fn render(snapshot: &Snapshot) -> serde_json::Result<String> {
    let mut text = serde_json::to_string_pretty(snapshot)?;
    text.push('\n');
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{TransportMode, TransportUsed};
    use crate::model::{ErrorKind, HostResult, HostStatus, ProbeErrorInfo};

    /// A26 contract: JSON keeps remote text verbatim; serde escapes C0 for the
    /// wire but leaves DEL and C1 alone, and the value round-trips unchanged.
    fn snapshot_with_remote_text(text: &str) -> Snapshot {
        let host = HostResult {
            host: "remote".into(),
            resolved_alias: "remote".into(),
            transport_used: TransportUsed::Direct,
            status: HostStatus::Error,
            probed_at: "2026-11-25T14:03:06+08:00".into(),
            duration_ms: 1,
            hostname: Some(text.into()),
            load1: None,
            cpus: None,
            fields_complete: false,
            processes_complete: false,
            note: Some(text.into()),
            gpus: Vec::new(),
            error: Some(ProbeErrorInfo::new(ErrorKind::Unknown, text)),
        };
        Snapshot::new(
            "2026-11-25T14:03:11+08:00".into(),
            None,
            TransportMode::Direct,
            1,
            vec![host],
        )
    }

    #[test]
    fn remote_text_is_raw_in_json_and_only_c0_is_escaped() {
        let raw = "esc\u{1b}]0;PWNED\u{7} del\u{7f} c1\u{9b}31m";
        let text = render(&snapshot_with_remote_text(raw)).unwrap();

        // C0 appears only as the two-character escape, never as a raw byte.
        assert!(!text.contains('\u{1b}'), "raw ESC on the wire");
        assert!(!text.contains('\u{7}'), "raw BEL on the wire");
        assert!(text.contains("\\u001b"));
        assert!(text.contains("\\u0007"));

        // serde_json does not escape DEL or C1 (RFC 8259 only requires C0).
        assert!(text.contains('\u{7f}'));
        assert!(text.contains('\u{9b}'));

        // A26: no sanitization happened — the value round-trips byte for byte.
        assert!(!text.contains('\u{fffd}'), "JSON must not rewrite text");
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["hosts"][0]["error"]["message"], raw);
        assert_eq!(value["hosts"][0]["hostname"], raw);
        assert_eq!(value["hosts"][0]["note"], raw);
    }
}
