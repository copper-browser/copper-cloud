//! Import format v1 lines (`application/x-ndjson`, one JSON object per line).
//!
//! Field order follows the contract so a human reading a captured stream sees `type` first.
//! Subjects are copper-cloud user ids rendered as hyphenated lower-case UUID strings — the
//! same subjects the link provisions, so canvas-cloud user ids equal copper user ids.

use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

/// Format version written on the `canvas` line.
pub const VERSION: u32 = 1;

/// Content type of an import body.
pub const CONTENT_TYPE: &str = "application/x-ndjson";

/// One line of an import body.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Line<'a> {
    Canvas {
        version: u32,
        id: Uuid,
        name: &'a str,
        kind: &'a str,
        owner: String,
        #[serde(with = "time::serde::rfc3339")]
        created_at: OffsetDateTime,
        #[serde(with = "time::serde::rfc3339")]
        updated_at: OffsetDateTime,
    },
    Member {
        subject: String,
        role: &'a str,
        #[serde(with = "time::serde::rfc3339")]
        added_at: OffsetDateTime,
    },
    Invite {
        id: Uuid,
        email: &'a str,
        invited_by: Option<String>,
        status: &'a str,
        #[serde(with = "time::serde::rfc3339")]
        created_at: OffsetDateTime,
        #[serde(with = "time::serde::rfc3339::option")]
        accepted_at: Option<OffsetDateTime>,
        #[serde(with = "time::serde::rfc3339::option")]
        nudged_at: Option<OffsetDateTime>,
        token_sha256_hex: &'a str,
    },
    Link {
        id: Uuid,
        token_sha256_hex: String,
        role: &'a str,
        created_by: Option<String>,
        #[serde(with = "time::serde::rfc3339")]
        created_at: OffsetDateTime,
        uses: i64,
    },
    Mention {
        id: Uuid,
        message_id: &'a str,
        from: String,
        to: String,
        excerpt: &'a str,
        #[serde(with = "time::serde::rfc3339")]
        created_at: OffsetDateTime,
        #[serde(with = "time::serde::rfc3339::option")]
        read_at: Option<OffsetDateTime>,
    },
    Update {
        b64: String,
    },
    End {
        updates: u64,
        members: u64,
        invites: u64,
        links: u64,
        mentions: u64,
    },
}

impl Line<'_> {
    /// The line as bytes, newline-terminated.
    pub fn to_bytes(&self) -> anyhow::Result<bytes::Bytes> {
        let mut out = serde_json::to_vec(self)?;
        out.push(b'\n');
        Ok(out.into())
    }
}

/// A user id as an import/app subject.
pub fn subject(id: Uuid) -> String {
    id.hyphenated().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_have_type_first_and_rfc3339_times() {
        let at = time::macros::datetime!(2026-10-19 18:00:00.25 UTC);
        let line = Line::Invite {
            id: Uuid::nil(),
            email: "a@example.com",
            invited_by: None,
            status: "pending",
            created_at: at,
            accepted_at: None,
            nudged_at: Some(at),
            token_sha256_hex: "ab",
        };
        let text = String::from_utf8(line.to_bytes().unwrap().to_vec()).unwrap();
        assert!(text.starts_with(r#"{"type":"invite","id":"00000000-0000-0000-0000-000000000000""#));
        assert!(text.contains(r#""invited_by":null"#));
        assert!(text.contains(r#""accepted_at":null"#));
        assert!(text.contains(r#""nudged_at":"2026-10-19T18:00:00.25Z""#));
        assert!(text.ends_with("}\n"));
        let end = Line::End {
            updates: 3,
            members: 2,
            invites: 1,
            links: 0,
            mentions: 4,
        };
        assert_eq!(
            end.to_bytes().unwrap(),
            &b"{\"type\":\"end\",\"updates\":3,\"members\":2,\"invites\":1,\"links\":0,\"mentions\":4}\n"[..]
        );
        assert_eq!(
            subject(Uuid::from_u128(0xA1)),
            "00000000-0000-0000-0000-0000000000a1"
        );
    }
}
