//! `tccd` record reading, against REAL captured log lines.
//!
//! The fixture below is verbatim `log show --predicate 'subsystem ==
//! "com.apple.TCC"'` output from this machine on 2026-10-08, three requests:
//! a live user denial of AppData attributed to Terminal while the ACCESSING
//! process was Claude Code, an allowed Photos request with no subject line at
//! all, and a policy denial. Real output, because every mistake this parser
//! could make is a mistake about a format nobody documents.

use env::tccd::{parse_log, Reason, Summary, Value};

const REAL: &str = r#"2026-10-08 15:55:48.233 Df tccd[21262:360dd88] [com.apple.TCC:access] AUTHREQ_CTX: msgID=21233.10240, function=TCCAccessRequest, service=kTCCServiceSystemPolicyAppData, preflight=no, query=1, client_dict=(null), daemon_dict=<private>
2026-10-08 15:55:48.233 Df tccd[21262:360dd88] [com.apple.TCC:access] AUTHREQ_ATTRIBUTION: msgID=21233.10240, attribution={responsible={TCCDProcess: identifier=com.apple.Terminal, pid=76460, auid=501, euid=501, responsible_path=/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal, binary_path=/opt/homebrew/Caskroom/claude-code@latest/2.1.280/claude}, accessing={TCCDProcess: identifier=com.anthropic.claude-code, pid=76460, auid=501, euid=501, binary_path=/opt/homebrew/Caskroom/claude-code@latest/2.1.280/claude}, requesting={TCCDProcess: identifier=com.apple.sandboxd, pid=21233, auid=0, euid=0, binary_path=/usr/libexec/sandboxd}, },
2026-10-08 15:55:48.235 Df tccd[21262:360dd88] [com.apple.TCC:access] AUTHREQ_SUBJECT: msgID=21233.10240, subject=com.apple.Terminal,
2026-10-08 15:55:48.235 Df tccd[21262:360dd88] [com.apple.TCC:access] AUTHREQ_RESULT: msgID=21233.10240, authValue=0, authReason=2, authVersion=1, desired_auth=0, error=(null),
2026-10-08 08:44:07.293 Df tccd[21262:34b4990] [com.apple.TCC:access] AUTHREQ_CTX: msgID=21184.257, function=<private>, service=kTCCServicePhotos, preflight=yes, query=1, client_dict=(null), daemon_dict=<private>
2026-10-08 08:44:07.293 Df tccd[21262:34b4990] [com.apple.TCC:access] AUTHREQ_ATTRIBUTION: msgID=21184.257, attribution={accessing={TCCDProcess: identifier=com.apple.mediaanalysisd, pid=18413, auid=501, euid=501, binary_path=/System/Library/PrivateFrameworks/MediaAnalysis.framework/Versions/A/mediaanalysisd}, requesting={TCCDProcess: identifier=com.apple.photolibraryd, pid=21184, auid=501, euid=501, binary_path=/System/Library/PrivateFrameworks/PhotoLibraryServices.framework/Versions/A/Support/photolibraryd}, },
2026-10-08 08:44:07.293 Df tccd[21262:34b4990] [com.apple.TCC:access] AUTHREQ_RESULT: msgID=21184.257, authValue=2, authReason=11, authVersion=1, desired_auth=0, error=(null),
2026-10-08 09:22:52.784 Df tccd[21252:34c58e7] [com.apple.TCC:access] AUTHREQ_CTX: msgID=498.32766, function=<private>, service=kTCCServiceDeveloperTool, preflight=yes, query=1, client_dict=(null), daemon_dict=<private>
2026-10-08 09:22:52.784 Df tccd[21252:34c58e7] [com.apple.TCC:access] AUTHREQ_ATTRIBUTION: msgID=498.32766, attribution={responsible={TCCDProcess: identifier=com.apple.Terminal, pid=26175, auid=501, euid=501, responsible_path=/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal, binary_path=/usr/bin/pgrep}, accessing={TCCDProcess: identifier=com.apple.pkill, pid=26175, auid=501, euid=501, binary_path=/usr/bin/pgrep}, requesting={TCCDProcess: identifier=com.apple.syspolicyd, pid=498, auid=0, euid=0, binary_path=/usr/libexec/syspolicyd}, },
2026-10-08 09:22:52.797 Df tccd[21252:34c58e7] [com.apple.TCC:access] AUTHREQ_SUBJECT: msgID=498.32766, subject=com.apple.Terminal,
2026-10-08 09:22:52.802 Df tccd[21252:34c58e7] [com.apple.TCC:access] AUTHREQ_RESULT: msgID=498.32766, authValue=0, authReason=4, authVersion=1, desired_auth=0, error=(null),
"#;

#[test]
fn the_three_identities_in_one_record_are_kept_apart() {
    let rs = parse_log(REAL);
    let d = rs
        .iter()
        .find(|r| r.service == "kTCCServiceSystemPolicyAppData")
        .expect("the AppData record must be parsed");

    // This is the whole point of the module: three DIFFERENT identities in one
    // record, and conflating them is how this was mis-diagnosed before.
    assert_eq!(d.subject, "com.apple.Terminal", "the prompt names the SUBJECT");
    assert_eq!(
        d.accessing, "com.anthropic.claude-code",
        "but the process that wanted the data is Claude Code"
    );
    assert_eq!(d.responsible, "com.apple.Terminal");
    assert_ne!(
        d.subject, d.accessing,
        "if these two were equal the parser would be reading one field twice"
    );

    assert_eq!(d.value, Value::Denied);
    assert_eq!(d.reason, Reason::UserConsent);
    assert!(d.is_live_denial(), "value=0 reason=2 is a denial he answered himself");
    assert!(!d.is_replay());
}

#[test]
fn a_policy_denial_is_not_counted_as_a_denial_he_made() {
    let rs = parse_log(REAL);
    let p = rs
        .iter()
        .find(|r| r.service == "kTCCServiceDeveloperTool")
        .expect("the policy record must be parsed");
    assert_eq!(p.value, Value::Denied, "it IS a denial");
    assert_eq!(p.reason, Reason::Other(4), "but nobody was asked");
    assert!(
        !p.is_live_denial(),
        "a policy denial must never enter the record of what HE refused -- that \
         record exists so he can see his own decisions, and padding it with the \
         system's would make his own unfindable"
    );

    // Positive sentinel: the live denial in the same fixture IS counted, so
    // `is_live_denial` discriminates rather than always being false.
    assert_eq!(
        rs.iter().filter(|r| r.is_live_denial()).count(),
        1,
        "exactly one of the three fixture records is a denial he made"
    );
}

#[test]
fn a_record_with_no_subject_line_falls_back_to_the_accessor() {
    let rs = parse_log(REAL);
    let a = rs
        .iter()
        .find(|r| r.service == "kTCCServicePhotos")
        .expect("the Photos record must be parsed");
    // This record genuinely has no AUTHREQ_SUBJECT line. The accessor must
    // still be read rather than the whole record being dropped.
    assert_eq!(a.accessing, "com.apple.mediaanalysisd");
    assert_eq!(a.value, Value::Allowed);
    assert_eq!(a.reason, Reason::Other(11));
    assert!(!a.is_live_denial() && !a.is_replay());
}

#[test]
fn a_request_with_no_result_line_is_dropped_rather_than_guessed() {
    // Three complete records in the fixture; adding a headless request must
    // not add a fourth.
    let before = parse_log(REAL).len();
    assert_eq!(before, 3, "the fixture holds exactly three complete records");

    let truncated = format!(
        "{REAL}\nAUTHREQ_CTX: msgID=999.1, service=kTCCServiceMicrophone\n\
         AUTHREQ_SUBJECT: msgID=999.1, subject=com.example.thing,\n"
    );
    assert_eq!(
        parse_log(&truncated).len(),
        before,
        "a request with no outcome is not evidence about an outcome"
    );

    // Positive sentinel: give that same request a result and it IS counted.
    let completed = format!(
        "{truncated}AUTHREQ_RESULT: msgID=999.1, authValue=0, authReason=2, authVersion=1\n"
    );
    let rs = parse_log(&completed);
    assert_eq!(rs.len(), before + 1, "a completed request must be counted");
    let new = rs.iter().find(|r| r.subject == "com.example.thing").unwrap();
    assert!(new.is_live_denial());
    assert_eq!(
        new.accessing, "com.example.thing",
        "with no accessing clause the subject is the accessor"
    );
    assert_eq!(
        new.responsible, "(none — its own subject)",
        "and a missing responsible clause says so rather than naming something"
    );
}

#[test]
fn the_summary_counts_denials_and_replays_separately() {
    let s = Summary::of(&parse_log(REAL));
    assert_eq!(s.total, 3);
    assert_eq!(s.live_denials, 1, "one of the three he answered by refusing");
    assert_eq!(s.replays, 0, "none of the three replayed a stored answer");
    assert_eq!(s.denial_groups().len(), 1);
    assert_eq!(s.denial_groups()[0].service, "kTCCServiceSystemPolicyAppData");
    assert_eq!(s.denial_groups()[0].count, 1);

    // Groups are most-frequent-first and the order is total, so two runs over
    // the same input agree.
    assert_eq!(s.groups.len(), 3, "three distinct groups from three records");
    let again = Summary::of(&parse_log(REAL));
    assert_eq!(s.groups, again.groups, "the grouping must be deterministic");
}

#[test]
fn the_durability_sentence_says_what_was_observed_not_what_it_means() {
    let s = Summary::of(&parse_log(REAL));
    let d = s.durability();
    assert!(d.contains("0 of 3"), "it must carry the numbers; got: {d}");
    assert!(d.contains("NOT replaying"), "and the observation; got: {d}");

    // With a replay present the sentence must change -- otherwise it is a
    // constant dressed up as a measurement.
    let with_replay = format!(
        "{REAL}\nAUTHREQ_CTX: msgID=777.1, service=kTCCServiceMicrophone\n\
         AUTHREQ_SUBJECT: msgID=777.1, subject=com.example.stored,\n\
         AUTHREQ_RESULT: msgID=777.1, authValue=2, authReason=3, authVersion=1\n"
    );
    let s2 = Summary::of(&parse_log(&with_replay));
    assert_eq!(s2.replays, 1);
    assert!(
        s2.durability().contains("replays at least sometimes"),
        "got: {}",
        s2.durability()
    );

    // And an empty window says it measured nothing, rather than reporting 0
    // replays as a finding.
    let empty = Summary::of(&[]);
    assert!(empty.durability().contains("nothing was measured"));
}

#[test]
fn an_identifier_is_read_from_its_own_clause_not_from_the_first_one_on_the_line() {
    // The attribution line carries up to three `identifier=` values. A parser
    // that searches the whole line returns whichever comes first and silently
    // conflates them. The AppData fixture has `responsible=` BEFORE
    // `accessing=`, so reading `accessing` correctly proves the scoping.
    let rs = parse_log(REAL);
    let d = rs.iter().find(|r| r.service == "kTCCServiceSystemPolicyAppData").unwrap();
    assert_eq!(d.responsible, "com.apple.Terminal");
    assert_eq!(d.accessing, "com.anthropic.claude-code");
    assert!(
        REAL.contains("responsible={TCCDProcess: identifier=com.apple.Terminal"),
        "sentinel: the fixture really does put responsible first"
    );
    let attr_line = REAL
        .lines()
        .find(|l| l.contains("AUTHREQ_ATTRIBUTION") && l.contains("AppData") == false && l.contains("21233.10240"))
        .expect("the attribution line must be in the fixture");
    assert!(
        attr_line.find("responsible={").unwrap() < attr_line.find("accessing={").unwrap(),
        "sentinel: responsible precedes accessing on the real line"
    );
}
