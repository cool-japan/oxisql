//! PostgreSQL logical-replication protocol commands.
//!
//! `IDENTIFY_SYSTEM`, `CREATE_REPLICATION_SLOT`, `DROP_REPLICATION_SLOT`, and
//! `START_REPLICATION` are not standard SQL statements — they belong to a
//! separate command grammar defined by the PostgreSQL **Streaming
//! Replication Protocol**.  A connection that has negotiated
//! `replication=database` (or `replication=true`) in its startup parameters
//! accepts these commands through the ordinary simple-query (`Q` message)
//! mechanism, and the server replies with an ordinary `RowDescription` /
//! `DataRow` / `CommandComplete` sequence (`START_REPLICATION` additionally
//! switches the connection into `CopyBoth` mode once the command starts
//! streaming).
//!
//! This module is **transport-agnostic**: it only builds command text from
//! typed Rust parameters, and parses already-extracted result-row field
//! values into typed results.  It does not send anything over the network
//! and does not depend on `tokio-postgres`, `postgres-protocol`, or any
//! wire-protocol message type.  A result row is modeled simply as
//! `&[Option<&str>]` — a row of nullable text-format field values, matching
//! how simple-query text-format results are actually shaped once decoded.
//! Sending the command and extracting those field values from the real
//! `RowDescription`/`DataRow` messages is the responsibility of the
//! connection-integration layer (a later wiring step), not this module.
//!
//! See the [PostgreSQL streaming replication protocol
//! documentation](https://www.postgresql.org/docs/current/protocol-replication.html)
//! for the authoritative command grammar this module implements.

use super::lsn::Lsn;
use crate::error::PgError;

// ── Identifier validation ─────────────────────────────────────────────────────

/// Maximum length, in bytes, of a PostgreSQL replication slot name.
///
/// Mirrors PostgreSQL's `NAMEDATALEN - 1` limit (`NAMEDATALEN` is 64,
/// including the trailing NUL) — the same bound the server enforces on slot
/// names internally.
const MAX_SLOT_NAME_LEN: usize = 63;

/// Validates a replication slot name.
///
/// PostgreSQL replication slot names must be non-empty, no longer than 63
/// bytes (`NAMEDATALEN - 1`), and consist only of lowercase ASCII letters,
/// digits, and underscores. This is stricter than a general SQL identifier
/// or channel name — in particular, unlike `LISTEN`/`NOTIFY` channel names,
/// uppercase letters are not permitted.
///
/// # Errors
///
/// Returns [`PgError::Replication`] if `name` is empty, longer than 63
/// bytes, or contains a character outside `[a-z0-9_]`.
pub(crate) fn validate_slot_name(name: &str) -> Result<(), PgError> {
    if name.is_empty() {
        return Err(PgError::Replication(
            "replication slot name must not be empty".to_string(),
        ));
    }
    if name.len() > MAX_SLOT_NAME_LEN {
        return Err(PgError::Replication(format!(
            "invalid replication slot name {name:?}: length {} exceeds the maximum of {MAX_SLOT_NAME_LEN} bytes",
            name.len()
        )));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(PgError::Replication(format!(
            "invalid replication slot name {name:?}: only lowercase ASCII letters, digits, and underscores are allowed"
        )));
    }
    Ok(())
}

/// Quotes and escapes a SQL identifier for interpolation into replication
/// command text.
///
/// Wraps `name` in double quotes and doubles any embedded double-quote
/// character, per standard SQL identifier-quoting rules (the same rule
/// PostgreSQL's own `quote_ident` applies). Unlike `validate_slot_name`,
/// this does not restrict the input character set — it is used both for
/// publication names (which may be mixed-case, arbitrary SQL identifiers)
/// and, defensively, for slot names that have already passed
/// `validate_slot_name`.
pub(crate) fn quote_publication_name(name: &str) -> String {
    let escaped = name.replace('"', "\"\"");
    format!("\"{escaped}\"")
}

// ── Command text builders ─────────────────────────────────────────────────────

/// Builds the `IDENTIFY_SYSTEM` command.
///
/// Takes no parameters; the command text is a fixed string that requests
/// the server's system identifier, current timeline, current WAL flush
/// position, and (if applicable) database name.
pub fn build_identify_system() -> String {
    "IDENTIFY_SYSTEM".to_string()
}

/// Builds a `CREATE_REPLICATION_SLOT` command for a logical slot using the
/// `pgoutput` output plugin.
///
/// If `temporary` is `true`, the `TEMPORARY` keyword is included and the
/// slot is dropped automatically when the session ends; otherwise the slot
/// persists across sessions until explicitly dropped.
///
/// # Errors
///
/// Returns [`PgError::Replication`] if `slot_name` fails
/// `validate_slot_name`.
pub fn build_create_replication_slot(slot_name: &str, temporary: bool) -> Result<String, PgError> {
    validate_slot_name(slot_name)?;
    let quoted_name = quote_publication_name(slot_name);
    let temp = if temporary { " TEMPORARY" } else { "" };
    Ok(format!(
        "CREATE_REPLICATION_SLOT {quoted_name}{temp} LOGICAL pgoutput"
    ))
}

/// Builds a `DROP_REPLICATION_SLOT` command.
///
/// # Errors
///
/// Returns [`PgError::Replication`] if `slot_name` fails
/// `validate_slot_name`.
pub fn build_drop_replication_slot(slot_name: &str) -> Result<String, PgError> {
    validate_slot_name(slot_name)?;
    let quoted_name = quote_publication_name(slot_name);
    Ok(format!("DROP_REPLICATION_SLOT {quoted_name}"))
}

/// Options for the parenthesized option list of a `START_REPLICATION`
/// command.
///
/// See [`build_start_replication`] for how each field maps onto the command
/// text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartReplicationOptions {
    /// The logical streaming protocol version to request. The MVP always
    /// passes `2` (the version that supports streaming of large
    /// in-progress transactions and two-phase commit).
    pub proto_version: u32,
    /// The publications to subscribe to. Must contain at least one name;
    /// each is quoted individually via `quote_publication_name` before
    /// being joined into the command's `publication_names` option value.
    pub publication_names: Vec<String>,
    /// Whether to request in-progress transaction streaming
    /// (`streaming 'on'`). The MVP always passes `false`.
    pub streaming: bool,
    /// Whether to request binary-format tuple data (`binary 'on'`). The
    /// MVP always passes `false`.
    pub binary: bool,
    /// Whether to request logical decoding messages emitted by
    /// `pg_logical_emit_message` (`messages 'on'`).
    pub messages: bool,
}

/// Builds a `START_REPLICATION` command for logical replication.
///
/// Produces command text of the form:
///
/// ```text
/// START_REPLICATION SLOT "slot_name" LOGICAL start_lsn (proto_version 'N', publication_names '"pub1", "pub2"'[, streaming 'on'][, binary 'on'][, messages 'on'])
/// ```
///
/// Each boolean option in `options` is included in the parenthesized list
/// only when `true`; when `false` it is omitted entirely rather than
/// written as e.g. `streaming 'off'` — an omitted option defaults to off on
/// the server, so the two forms are equivalent and omission is simpler.
///
/// # Errors
///
/// Returns [`PgError::Replication`] if:
/// - `slot_name` fails `validate_slot_name`;
/// - `options.publication_names` is empty (`START_REPLICATION LOGICAL`
///   without at least one publication is meaningless); or
/// - any entry in `options.publication_names` is an empty string.
pub fn build_start_replication(
    slot_name: &str,
    start_lsn: Lsn,
    options: &StartReplicationOptions,
) -> Result<String, PgError> {
    validate_slot_name(slot_name)?;
    if options.publication_names.is_empty() {
        return Err(PgError::Replication(
            "START_REPLICATION requires at least one publication name".to_string(),
        ));
    }
    for name in &options.publication_names {
        if name.is_empty() {
            return Err(PgError::Replication(
                "publication name must not be empty".to_string(),
            ));
        }
    }

    let quoted_slot = quote_publication_name(slot_name);
    let publication_list = options
        .publication_names
        .iter()
        .map(|name| quote_publication_name(name))
        .collect::<Vec<_>>()
        .join(", ");

    let mut command_options = format!(
        "proto_version '{}', publication_names '{publication_list}'",
        options.proto_version
    );
    if options.streaming {
        command_options.push_str(", streaming 'on'");
    }
    if options.binary {
        command_options.push_str(", binary 'on'");
    }
    if options.messages {
        command_options.push_str(", messages 'on'");
    }

    Ok(format!(
        "START_REPLICATION SLOT {quoted_slot} LOGICAL {start_lsn} ({command_options})"
    ))
}

// ── Result row types + parsers ────────────────────────────────────────────────

/// The parsed result row of an `IDENTIFY_SYSTEM` command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentifySystem {
    /// The database system identifier (a decimal-formatted 64-bit value,
    /// kept as text since it does not fit meaningfully into any numeric
    /// type used elsewhere in this crate).
    pub systemid: String,
    /// The current timeline ID.
    pub timeline: u32,
    /// The current WAL flush position of the server.
    pub xlogpos: Lsn,
    /// The database name the connection is associated with, or `None` for
    /// a connection in physical-replication mode (no database selected).
    pub dbname: Option<String>,
}

/// Parses the single result row returned by `IDENTIFY_SYSTEM`.
///
/// Expects exactly 4 fields, in server order: `systemid`, `timeline`,
/// `xlogpos`, `dbname`. The first three are required (non-`NULL`);
/// `dbname` is nullable (`NULL` for a physical-replication connection).
///
/// # Errors
///
/// Returns [`PgError::Protocol`] if:
/// - `fields` does not contain exactly 4 elements;
/// - `systemid`, `timeline`, or `xlogpos` is `None` (`NULL`);
/// - `timeline` does not parse as a `u32`; or
/// - `xlogpos` does not parse as an `Lsn`.
pub fn parse_identify_system_row(fields: &[Option<&str>]) -> Result<IdentifySystem, PgError> {
    if fields.len() != 4 {
        return Err(PgError::Protocol(format!(
            "IDENTIFY_SYSTEM: expected 4 fields (systemid, timeline, xlogpos, dbname), got {}",
            fields.len()
        )));
    }

    let systemid = fields[0].ok_or_else(|| {
        PgError::Protocol("IDENTIFY_SYSTEM: 'systemid' field is NULL, expected text".to_string())
    })?;

    let timeline_text = fields[1].ok_or_else(|| {
        PgError::Protocol("IDENTIFY_SYSTEM: 'timeline' field is NULL, expected text".to_string())
    })?;
    let timeline: u32 = timeline_text.parse::<u32>().map_err(|e| {
        PgError::Protocol(format!(
            "IDENTIFY_SYSTEM: 'timeline' field {timeline_text:?} is not a valid u32: {e}"
        ))
    })?;

    let xlogpos_text = fields[2].ok_or_else(|| {
        PgError::Protocol("IDENTIFY_SYSTEM: 'xlogpos' field is NULL, expected text".to_string())
    })?;
    let xlogpos = xlogpos_text.parse::<Lsn>().map_err(|e| {
        PgError::Protocol(format!(
            "IDENTIFY_SYSTEM: 'xlogpos' field {xlogpos_text:?} is not a valid LSN: {e}"
        ))
    })?;

    let dbname = fields[3].map(str::to_string);

    Ok(IdentifySystem {
        systemid: systemid.to_string(),
        timeline,
        xlogpos,
        dbname,
    })
}

/// The parsed result row of a `CREATE_REPLICATION_SLOT` command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedSlot {
    /// The (possibly server-normalized) name of the created slot.
    pub slot_name: String,
    /// The WAL position at which the slot becomes consistent — logical
    /// decoding should begin from this position.
    pub consistent_point: Lsn,
    /// The name of the exported snapshot that reflects the state at
    /// `consistent_point`, or `None` if no snapshot was exported (e.g.
    /// `NOEXPORT_SNAPSHOT` was requested, or the slot is temporary).
    pub snapshot_name: Option<String>,
    /// The output plugin used by the slot, or `None` for a physical slot.
    pub output_plugin: Option<String>,
}

/// Parses the single result row returned by `CREATE_REPLICATION_SLOT`.
///
/// Expects exactly 4 fields, in server order: `slot_name`,
/// `consistent_point`, `snapshot_name`, `output_plugin`. The first two are
/// required (non-`NULL`); the latter two are nullable.
///
/// # Errors
///
/// Returns [`PgError::Protocol`] if:
/// - `fields` does not contain exactly 4 elements;
/// - `slot_name` or `consistent_point` is `None` (`NULL`); or
/// - `consistent_point` does not parse as an `Lsn`.
pub fn parse_create_replication_slot_row(fields: &[Option<&str>]) -> Result<CreatedSlot, PgError> {
    if fields.len() != 4 {
        return Err(PgError::Protocol(format!(
            "CREATE_REPLICATION_SLOT: expected 4 fields (slot_name, consistent_point, snapshot_name, output_plugin), got {}",
            fields.len()
        )));
    }

    let slot_name = fields[0].ok_or_else(|| {
        PgError::Protocol(
            "CREATE_REPLICATION_SLOT: 'slot_name' field is NULL, expected text".to_string(),
        )
    })?;

    let consistent_point_text = fields[1].ok_or_else(|| {
        PgError::Protocol(
            "CREATE_REPLICATION_SLOT: 'consistent_point' field is NULL, expected text".to_string(),
        )
    })?;
    let consistent_point = consistent_point_text.parse::<Lsn>().map_err(|e| {
        PgError::Protocol(format!(
            "CREATE_REPLICATION_SLOT: 'consistent_point' field {consistent_point_text:?} is not a valid LSN: {e}"
        ))
    })?;

    let snapshot_name = fields[2].map(str::to_string);
    let output_plugin = fields[3].map(str::to_string);

    Ok(CreatedSlot {
        slot_name: slot_name.to_string(),
        consistent_point,
        snapshot_name,
        output_plugin,
    })
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── validate_slot_name ──────────────────────────────────────────────────

    #[test]
    fn validate_slot_name_accepts_simple_name() {
        assert!(validate_slot_name("my_slot").is_ok());
    }

    #[test]
    fn validate_slot_name_accepts_single_char() {
        assert!(validate_slot_name("a").is_ok());
    }

    #[test]
    fn validate_slot_name_accepts_all_lowercase_alnum_underscore() {
        assert!(validate_slot_name("abc_123_xyz_0").is_ok());
    }

    #[test]
    fn validate_slot_name_accepts_exactly_63_chars() {
        let name = "a".repeat(63);
        assert_eq!(name.len(), 63);
        assert!(validate_slot_name(&name).is_ok());
    }

    #[test]
    fn validate_slot_name_rejects_empty() {
        assert!(matches!(
            validate_slot_name(""),
            Err(PgError::Replication(_))
        ));
    }

    #[test]
    fn validate_slot_name_rejects_64_chars() {
        let name = "a".repeat(64);
        assert!(matches!(
            validate_slot_name(&name),
            Err(PgError::Replication(_))
        ));
    }

    #[test]
    fn validate_slot_name_rejects_uppercase() {
        assert!(matches!(
            validate_slot_name("MySlot"),
            Err(PgError::Replication(_))
        ));
    }

    #[test]
    fn validate_slot_name_rejects_hyphen() {
        assert!(matches!(
            validate_slot_name("my-slot"),
            Err(PgError::Replication(_))
        ));
    }

    #[test]
    fn validate_slot_name_rejects_space() {
        assert!(matches!(
            validate_slot_name("my slot"),
            Err(PgError::Replication(_))
        ));
    }

    #[test]
    fn validate_slot_name_rejects_double_quote() {
        assert!(matches!(
            validate_slot_name("my\"slot"),
            Err(PgError::Replication(_))
        ));
    }

    #[test]
    fn validate_slot_name_rejects_semicolon() {
        assert!(matches!(
            validate_slot_name("my;slot"),
            Err(PgError::Replication(_))
        ));
    }

    #[test]
    fn validate_slot_name_rejects_sql_comment() {
        assert!(matches!(
            validate_slot_name("my--slot"),
            Err(PgError::Replication(_))
        ));
    }

    // ── quote_publication_name ───────────────────────────────────────────────

    #[test]
    fn quote_publication_name_plain() {
        assert_eq!(quote_publication_name("my_pub"), "\"my_pub\"");
    }

    #[test]
    fn quote_publication_name_embedded_quote_is_doubled() {
        assert_eq!(quote_publication_name("weird\"pub"), "\"weird\"\"pub\"");
    }

    #[test]
    fn quote_publication_name_other_characters_pass_through() {
        assert_eq!(
            quote_publication_name("Mixed-Case Pub.1"),
            "\"Mixed-Case Pub.1\""
        );
    }

    // ── build_create_replication_slot ────────────────────────────────────────

    #[test]
    fn build_create_replication_slot_permanent() {
        let cmd = build_create_replication_slot("myslot", false).unwrap();
        assert_eq!(cmd, "CREATE_REPLICATION_SLOT \"myslot\" LOGICAL pgoutput");
    }

    #[test]
    fn build_create_replication_slot_temporary() {
        let cmd = build_create_replication_slot("myslot", true).unwrap();
        assert_eq!(
            cmd,
            "CREATE_REPLICATION_SLOT \"myslot\" TEMPORARY LOGICAL pgoutput"
        );
    }

    #[test]
    fn build_create_replication_slot_rejects_invalid_name() {
        assert!(matches!(
            build_create_replication_slot("Invalid-Slot", false),
            Err(PgError::Replication(_))
        ));
    }

    // ── build_drop_replication_slot ──────────────────────────────────────────

    #[test]
    fn build_drop_replication_slot_happy_path() {
        let cmd = build_drop_replication_slot("myslot").unwrap();
        assert_eq!(cmd, "DROP_REPLICATION_SLOT \"myslot\"");
    }

    #[test]
    fn build_drop_replication_slot_rejects_invalid_name() {
        assert!(matches!(
            build_drop_replication_slot("Invalid-Slot"),
            Err(PgError::Replication(_))
        ));
    }

    // ── build_start_replication ──────────────────────────────────────────────

    #[test]
    fn build_start_replication_minimal_options_exact_string() {
        let options = StartReplicationOptions {
            proto_version: 2,
            publication_names: vec!["my_pub".to_string()],
            streaming: false,
            binary: false,
            messages: false,
        };
        let cmd = build_start_replication("myslot", Lsn::from_u64(0), &options).unwrap();
        assert_eq!(
            cmd,
            "START_REPLICATION SLOT \"myslot\" LOGICAL 0/0 (proto_version '2', publication_names '\"my_pub\"')"
        );
    }

    #[test]
    fn build_start_replication_multiple_publications_comma_joined() {
        let options = StartReplicationOptions {
            proto_version: 2,
            publication_names: vec!["pub_a".to_string(), "pub_b".to_string()],
            streaming: false,
            binary: false,
            messages: false,
        };
        let cmd = build_start_replication("myslot", Lsn::from_u64(0), &options).unwrap();
        assert_eq!(
            cmd,
            "START_REPLICATION SLOT \"myslot\" LOGICAL 0/0 (proto_version '2', publication_names '\"pub_a\", \"pub_b\"')"
        );
    }

    #[test]
    fn build_start_replication_all_boolean_options_on() {
        let options = StartReplicationOptions {
            proto_version: 2,
            publication_names: vec!["my_pub".to_string()],
            streaming: true,
            binary: true,
            messages: true,
        };
        let cmd = build_start_replication("myslot", Lsn::from_u64(0), &options).unwrap();
        assert!(cmd.contains("streaming 'on'"));
        assert!(cmd.contains("binary 'on'"));
        assert!(cmd.contains("messages 'on'"));
        assert_eq!(
            cmd,
            "START_REPLICATION SLOT \"myslot\" LOGICAL 0/0 (proto_version '2', publication_names '\"my_pub\"', streaming 'on', binary 'on', messages 'on')"
        );
    }

    #[test]
    fn build_start_replication_uses_start_lsn() {
        let options = StartReplicationOptions {
            proto_version: 2,
            publication_names: vec!["my_pub".to_string()],
            streaming: false,
            binary: false,
            messages: false,
        };
        let lsn: Lsn = "16/B374D848".parse().unwrap();
        let cmd = build_start_replication("myslot", lsn, &options).unwrap();
        assert!(cmd.contains("LOGICAL 16/B374D848 ("));
    }

    #[test]
    fn build_start_replication_rejects_invalid_slot_name() {
        let options = StartReplicationOptions {
            proto_version: 2,
            publication_names: vec!["my_pub".to_string()],
            streaming: false,
            binary: false,
            messages: false,
        };
        assert!(matches!(
            build_start_replication("Invalid-Slot", Lsn::from_u64(0), &options),
            Err(PgError::Replication(_))
        ));
    }

    #[test]
    fn build_start_replication_rejects_empty_publication_list() {
        let options = StartReplicationOptions {
            proto_version: 2,
            publication_names: vec![],
            streaming: false,
            binary: false,
            messages: false,
        };
        assert!(matches!(
            build_start_replication("myslot", Lsn::from_u64(0), &options),
            Err(PgError::Replication(_))
        ));
    }

    #[test]
    fn build_start_replication_rejects_empty_publication_name_entry() {
        let options = StartReplicationOptions {
            proto_version: 2,
            publication_names: vec!["good_pub".to_string(), String::new()],
            streaming: false,
            binary: false,
            messages: false,
        };
        assert!(matches!(
            build_start_replication("myslot", Lsn::from_u64(0), &options),
            Err(PgError::Replication(_))
        ));
    }

    // ── parse_identify_system_row ────────────────────────────────────────────

    #[test]
    fn parse_identify_system_row_valid_full_row() {
        let fields = [
            Some("6821810470617038336"),
            Some("1"),
            Some("16/B374D848"),
            Some("postgres"),
        ];
        let result = parse_identify_system_row(&fields).unwrap();
        assert_eq!(result.systemid, "6821810470617038336");
        assert_eq!(result.timeline, 1);
        assert_eq!(result.xlogpos, "16/B374D848".parse::<Lsn>().unwrap());
        assert_eq!(result.dbname, Some("postgres".to_string()));
    }

    #[test]
    fn parse_identify_system_row_dbname_none_is_valid() {
        let fields = [
            Some("6821810470617038336"),
            Some("1"),
            Some("16/B374D848"),
            None,
        ];
        let result = parse_identify_system_row(&fields).unwrap();
        assert_eq!(result.dbname, None);
    }

    #[test]
    fn parse_identify_system_row_wrong_field_count_too_few() {
        let fields = [Some("123"), Some("1"), Some("16/B374D848")];
        assert!(matches!(
            parse_identify_system_row(&fields),
            Err(PgError::Protocol(_))
        ));
    }

    #[test]
    fn parse_identify_system_row_wrong_field_count_too_many() {
        let fields = [
            Some("123"),
            Some("1"),
            Some("16/B374D848"),
            Some("postgres"),
            Some("extra"),
        ];
        assert!(matches!(
            parse_identify_system_row(&fields),
            Err(PgError::Protocol(_))
        ));
    }

    #[test]
    fn parse_identify_system_row_non_numeric_timeline() {
        let fields = [
            Some("123"),
            Some("not_a_number"),
            Some("16/B374D848"),
            Some("postgres"),
        ];
        assert!(matches!(
            parse_identify_system_row(&fields),
            Err(PgError::Protocol(_))
        ));
    }

    #[test]
    fn parse_identify_system_row_malformed_xlogpos() {
        let fields = [Some("123"), Some("1"), Some("not-an-lsn"), Some("postgres")];
        assert!(matches!(
            parse_identify_system_row(&fields),
            Err(PgError::Protocol(_))
        ));
    }

    #[test]
    fn parse_identify_system_row_required_field_null() {
        let fields = [None, Some("1"), Some("16/B374D848"), Some("postgres")];
        assert!(matches!(
            parse_identify_system_row(&fields),
            Err(PgError::Protocol(_))
        ));
    }

    // ── parse_create_replication_slot_row ────────────────────────────────────

    #[test]
    fn parse_create_replication_slot_row_valid_full_row() {
        let fields = [
            Some("myslot"),
            Some("16/B374D848"),
            Some("00000003-0000-0004-1-0"),
            Some("pgoutput"),
        ];
        let result = parse_create_replication_slot_row(&fields).unwrap();
        assert_eq!(result.slot_name, "myslot");
        assert_eq!(
            result.consistent_point,
            "16/B374D848".parse::<Lsn>().unwrap()
        );
        assert_eq!(
            result.snapshot_name,
            Some("00000003-0000-0004-1-0".to_string())
        );
        assert_eq!(result.output_plugin, Some("pgoutput".to_string()));
    }

    #[test]
    fn parse_create_replication_slot_row_null_snapshot_and_plugin() {
        // e.g. the NOEXPORT_SNAPSHOT case.
        let fields = [Some("myslot"), Some("16/B374D848"), None, None];
        let result = parse_create_replication_slot_row(&fields).unwrap();
        assert_eq!(result.snapshot_name, None);
        assert_eq!(result.output_plugin, None);
    }

    #[test]
    fn parse_create_replication_slot_row_wrong_field_count() {
        let fields = [Some("myslot"), Some("16/B374D848")];
        assert!(matches!(
            parse_create_replication_slot_row(&fields),
            Err(PgError::Protocol(_))
        ));
    }

    #[test]
    fn parse_create_replication_slot_row_malformed_consistent_point() {
        let fields = [Some("myslot"), Some("not-an-lsn"), None, Some("pgoutput")];
        assert!(matches!(
            parse_create_replication_slot_row(&fields),
            Err(PgError::Protocol(_))
        ));
    }

    #[test]
    fn parse_create_replication_slot_row_required_field_null() {
        let fields = [None, Some("16/B374D848"), None, Some("pgoutput")];
        assert!(matches!(
            parse_create_replication_slot_row(&fields),
            Err(PgError::Protocol(_))
        ));
    }
}
