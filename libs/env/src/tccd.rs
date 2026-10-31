//! **Reading macOS's own permission records** — who is actually asking, and
//! what the operator has already refused.
//!
//! The operator's ask, verbatim: *"I need a way to see what is actually
//! requesting the permission and give fine-grained access, because macos just
//! says 'Claude is requesting to access data from other apps' over and over,
//! and the truth is more nuanced."* And, separately and still unbuilt: *"I need
//! to track which things I am denying access to because I do not want rogue AI
//! scanning my whole disk all the time."*
//!
//! Both are answered by the same source, and it needs no privilege and no
//! grant: `tccd` writes a four-line record for every authorisation request into
//! the unified log, and any user can read it back.
//!
//! # The four lines, and why all four are needed
//!
//! A single request is spread across four entries keyed by `msgID`:
//!
//! ```text
//! AUTHREQ_CTX:         msgID=X, ... service=kTCCServiceSystemPolicyAppData
//! AUTHREQ_ATTRIBUTION: msgID=X, attribution={responsible={...}, accessing={...}, requesting={...}}
//! AUTHREQ_SUBJECT:     msgID=X, subject=com.apple.Terminal
//! AUTHREQ_RESULT:      msgID=X, authValue=0, authReason=2, ...
//! ```
//!
//! The distinction between those identities is the whole of the "more nuanced"
//! the operator asked for, and getting it wrong is how this was mis-diagnosed
//! before:
//!
//! * **accessing** is the process that wants the data. Usually the interesting
//!   one to a human: `com.anthropic.claude-code`.
//! * **responsible** is the process macOS holds accountable — inherited down a
//!   whole process tree unless a child disclaims it.
//! * **subject** is the identity the prompt NAMES and the identity consent is
//!   keyed to. It follows *responsible*, not *accessing*, which is why signing
//!   a leaf binary changes nothing about the name on the dialog.
//!
//! # Why `authReason` carries the finding
//!
//! [`Reason::UserConsent`] (2) means a dialog was shown and the operator
//! answered it — every one of those is an interruption. [`Reason::UserSet`] (3)
//! means a *stored* decision was replayed, with no dialog. So the ratio between
//! them is the measurement of whether consent is durable at all, and it can be
//! taken passively, from records that already exist, without provoking a single
//! prompt.

use std::collections::BTreeMap;

/// What `tccd` decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Value {
    Denied,
    Unknown,
    Allowed,
    Limited,
    Other(i64),
}

impl Value {
    pub fn from_code(c: i64) -> Self {
        match c {
            0 => Value::Denied,
            1 => Value::Unknown,
            2 => Value::Allowed,
            3 => Value::Limited,
            other => Value::Other(other),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Value::Denied => "denied",
            Value::Unknown => "unknown",
            Value::Allowed => "allowed",
            Value::Limited => "limited",
            Value::Other(_) => "other",
        }
    }
}

/// Why it decided that. Only the two that carry the durability finding are
/// named; the rest stay numeric rather than being guessed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reason {
    /// A dialog was shown and the operator answered it. An interruption.
    UserConsent,
    /// A stored decision was replayed. No dialog, no interruption. The thing
    /// that has never yet been observed on this machine for the AppData
    /// service.
    UserSet,
    Other(i64),
}

impl Reason {
    pub fn from_code(c: i64) -> Self {
        match c {
            2 => Reason::UserConsent,
            3 => Reason::UserSet,
            other => Reason::Other(other),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Reason::UserConsent => "he was asked",
            Reason::UserSet => "replayed from a stored answer",
            Reason::Other(_) => "set by policy",
        }
    }
}

/// One correlated authorisation request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// `kTCCService…`, the permission class.
    pub service: String,
    /// The identity the PROMPT NAMES and consent is keyed to.
    pub subject: String,
    /// The process that wanted the data.
    pub accessing: String,
    /// The process macOS held accountable.
    pub responsible: String,
    pub value: Value,
    pub reason: Reason,
}

impl Record {
    /// Whether this record is an interruption the operator personally answered
    /// by refusing. This is the set the denial record is made of.
    pub fn is_live_denial(&self) -> bool {
        self.value == Value::Denied && self.reason == Reason::UserConsent
    }

    /// Whether a stored answer was replayed, so no dialog was shown.
    pub fn is_replay(&self) -> bool {
        self.reason == Reason::UserSet
    }
}

/// Parse `log show --predicate 'subsystem == "com.apple.TCC"'` output.
///
/// A pure function over text so the suite can force every branch off real
/// captured log lines rather than needing a live machine in a given state.
/// Records missing the `AUTHREQ_RESULT` line are dropped: a request with no
/// outcome is not evidence about an outcome.
pub fn parse_log(text: &str) -> Vec<Record> {
    #[derive(Default)]
    struct Partial {
        service: Option<String>,
        subject: Option<String>,
        accessing: Option<String>,
        responsible: Option<String>,
        result: Option<(i64, i64)>,
    }

    // BTreeMap so the output order is deterministic and a diff of two runs is
    // readable.
    let mut by_id: BTreeMap<String, Partial> = BTreeMap::new();

    for line in text.lines() {
        let Some((tag, rest)) = split_tag(line) else { continue };
        let Some(id) = field(rest, "msgID=") else { continue };
        let e = by_id.entry(id).or_default();
        match tag {
            "AUTHREQ_CTX" => e.service = field(rest, "service="),
            "AUTHREQ_SUBJECT" => e.subject = field(rest, "subject="),
            "AUTHREQ_ATTRIBUTION" => {
                e.accessing = clause_identifier(rest, "accessing={");
                e.responsible = clause_identifier(rest, "responsible={");
            }
            "AUTHREQ_RESULT" => {
                let v = field(rest, "authValue=").and_then(|s| s.parse().ok());
                let r = field(rest, "authReason=").and_then(|s| s.parse().ok());
                if let (Some(v), Some(r)) = (v, r) {
                    e.result = Some((v, r));
                }
            }
            _ => {}
        }
    }

    by_id
        .into_values()
        .filter_map(|p| {
            let (v, r) = p.result?;
            let subject = p.subject.unwrap_or_else(|| "(unrecorded)".into());
            Some(Record {
                service: p.service.unwrap_or_else(|| "(unrecorded)".into()),
                // `accessing` is absent on a request a process makes for
                // itself; the subject is then the accessor.
                accessing: p.accessing.unwrap_or_else(|| subject.clone()),
                subject,
                responsible: p.responsible.unwrap_or_else(|| "(none — its own subject)".into()),
                value: Value::from_code(v),
                reason: Reason::from_code(r),
            })
        })
        .collect()
}

/// Find `AUTHREQ_*:` in a compact log line and return the tag and the rest.
fn split_tag(line: &str) -> Option<(&str, &str)> {
    let at = line.find("AUTHREQ_")?;
    let rest = &line[at..];
    let colon = rest.find(':')?;
    Some((&rest[..colon], &rest[colon + 1..]))
}

/// The value of `key=` up to the next `,` — trimmed, and `None` when empty.
fn field(hay: &str, key: &str) -> Option<String> {
    let rest = hay.split_once(key)?.1;
    let end = rest.find(',').unwrap_or(rest.len());
    let v = rest[..end].trim().trim_end_matches(',').trim();
    (!v.is_empty()).then(|| v.to_string())
}

/// The `identifier=` inside one `…={TCCDProcess: …}` clause of an attribution.
///
/// Scoped to the clause on purpose: the three clauses each carry their own
/// `identifier=`, so a search over the whole line would return whichever came
/// first and silently conflate the three identities this module exists to keep
/// apart.
fn clause_identifier(hay: &str, clause: &str) -> Option<String> {
    let rest = hay.split_once(clause)?.1;
    let end = rest.find('}').unwrap_or(rest.len());
    field(&rest[..end], "identifier=")
}

/// A counted group, for a report ordered by how often it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub count: usize,
    pub subject: String,
    pub accessing: String,
    pub service: String,
    pub value: Value,
    pub reason: Reason,
}

/// What a window of records adds up to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub total: usize,
    /// Every distinct (subject, accessing, service, outcome), most frequent
    /// first.
    pub groups: Vec<Group>,
    /// Requests the operator personally answered by refusing.
    pub live_denials: usize,
    /// Requests answered from a stored decision, with no dialog shown.
    pub replays: usize,
}

impl Summary {
    pub fn of(records: &[Record]) -> Self {
        let mut counts: BTreeMap<(String, String, String, Value, Reason), usize> =
            BTreeMap::new();
        for r in records {
            *counts
                .entry((
                    r.subject.clone(),
                    r.accessing.clone(),
                    r.service.clone(),
                    r.value,
                    r.reason,
                ))
                .or_default() += 1;
        }
        let mut groups: Vec<Group> = counts
            .into_iter()
            .map(|((subject, accessing, service, value, reason), count)| Group {
                count,
                subject,
                accessing,
                service,
                value,
                reason,
            })
            .collect();
        // Most frequent first; ties broken by the group's own fields so the
        // order is total and two runs over the same input agree.
        groups.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.subject.cmp(&b.subject))
                .then_with(|| a.service.cmp(&b.service))
                .then_with(|| a.accessing.cmp(&b.accessing))
        });

        Self {
            total: records.len(),
            live_denials: records.iter().filter(|r| r.is_live_denial()).count(),
            replays: records.iter().filter(|r| r.is_replay()).count(),
            groups,
        }
    }

    /// Only the groups that are live denials — the record the operator asked
    /// for, which is about what he has REFUSED and not about traffic.
    pub fn denial_groups(&self) -> Vec<&Group> {
        self.groups
            .iter()
            .filter(|g| g.value == Value::Denied && g.reason == Reason::UserConsent)
            .collect()
    }

    /// The durability verdict, in one sentence, stated as what was observed
    /// rather than as a conclusion.
    ///
    /// This is deliberately not phrased as "permissions are broken": the
    /// standing instruction on this subject is that nothing here is reported
    /// solved — or diagnosed — on anything but a measurement.
    pub fn durability(&self) -> String {
        if self.total == 0 {
            return "no requests in the window, so nothing was measured".to_string();
        }
        if self.replays == 0 {
            format!(
                "0 of {} requests were answered from a stored decision. Every answered \
                 request in this window interrupted somebody. Consent is NOT replaying.",
                self.total
            )
        } else {
            format!(
                "{} of {} requests were answered from a stored decision, so consent \
                 replays at least sometimes.",
                self.replays, self.total
            )
        }
    }
}
