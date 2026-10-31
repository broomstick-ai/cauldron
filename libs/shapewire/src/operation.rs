//! # What an operation TAKES — a closed set of payload dtypes, checked by the compiler
//!
//! One leaf of a producer's surface — `threads/{thread_id}/send`,
//! `budgets/{key}/set` — declares a path and the payload it accepts. This module
//! is the vocabulary those declarations are written in: twelve dtypes, three
//! presences, and a `const fn` that refuses a malformed declaration while the
//! producer is still being compiled.
//!
//! ## Why this is in the unflagged floor
//!
//! It sits beside `wire`, `mirror`, `envelope`, `cell` and `surface` with no
//! feature flag, and that placement is a decision rather than an oversight:
//!
//! * **A CONSUMER is the reader.** Knowing that `threads/{id}/update` takes a
//!   delta of `ThreadSettings` is exactly what a face needs before it can
//!   construct a call. The declaration is consumer-facing information that
//!   happens to be authored on the producer side.
//! * **It costs nothing to link.** Everything here is `const` data and `const fn`
//!   over `&'static str`. No runtime, no socket, no clock, not one dependency
//!   outside `core`/`std` — the floor's whole premise.
//! * **Behind the `server` flag it would have been unreachable.** A face would
//!   have had to link the entire network door, router and all, to find out what a
//!   payload looks like. That is the trade this layout refuses, and it is the
//!   same argument that put the frame types in the floor instead of leaving them
//!   wherever the file they were extracted from happened to end.
//!
//! ## Why the set can be CLOSED at all
//!
//! Operations and resources are single-purpose leaves whose context is their
//! parent shape. A leaf never has to carry its own context, so a payload carries
//! a VALUE and nothing else — and the number of kinds of value a leaf can take is
//! small and finite. A payload that needed a thirteenth kind would be a leaf
//! whose context is NOT its parent, which the shape design rules already classify
//! as an unrecognised shape. So a new member is a finding about the model first
//! and an edit to [`DType`] second.
//!
//! ## Compile time, not run time
//!
//! The ruling this implements: an app must support the chassis declarations, and
//! when a shape is applied to a chassis the producer has to implement it, with
//! the check running at compile time. `declare_operation!` emits the `Operation`
//! impl AND a `const` assertion over [`well_formed`], so a malformed declaration
//! is a build failure in the producer's own crate. **Nothing in this module ever
//! looks at a request.** There is no validator here and no error type here.
//!
//! ## The set was derived from real operations, and the first cut was wrong
//!
//! The candidate list was `scalar · enum · struct · list · ref · blob · delta`.
//! Four corrections survive in the code, each one worth more than the enum it
//! produced:
//!
//! * **`blob` has no instance, so it is not a member.** No bytes, base64 or
//!   file-body parameter exists anywhere on the surface, and the producer that
//!   drove this derivation implements neither the resource nor the upload door —
//!   so the blob channel is designed and unbuilt. A member whose only
//!   justification is a document describes nothing.
//! * **`ref` split in two.** The candidate glossed it as "a path", which is two
//!   different facts wearing one name. [`DType::Ref`] is an id in the producer's
//!   OWN address space and a declaration can name what it refers to;
//!   [`DType::Path`] is a name in the host filesystem, resolvable only outside
//!   the producer and checkable from no declaration. A consumer that treated the
//!   two alike would be building filesystem paths out of thread ids.
//! * **Four members were missing**, each with an operation that cannot be
//!   declared honestly without it: [`DType::Opaque`], [`DType::Timestamp`],
//!   [`DType::Duration`], [`DType::Cursor`]. What each one buys is on its
//!   variant.
//! * **`delta` needs a named subtype** — see below.
//!
//! ## `delta` carries a name because `null` means different things
//!
//! `threads/{thread_id}/update` takes `patch: SettingsPatch`, the only
//! first-class delta on that surface. Six of its fields are `Option<Option<T>>`
//! deserialized through a `some_nullable` helper, so ABSENT means *leave this
//! alone* and an explicit `null` means *clear it*. That rule is not a property of
//! being a delta; it is a property of being a delta OF `ThreadSettings`. A
//! consumer told only `delta` would not know that `null` clears, and the next
//! delta on this surface is free to spell it differently. So [`DType::Delta`]
//! names what it patches and [`well_formed`] refuses an empty name.
//!
//! The brief that commissioned this work also named a `file_delta` instance. A
//! grep over the producer finds no `file_delta`, no diff and no patch-of-a-file
//! anywhere; the code wins over the brief, and the one shipped instance is
//! `Delta("ThreadSettings")`.
//!
//! ## The hole, stated rather than hidden
//!
//! A declaration is checked for WELL-FORMEDNESS and for nothing else. **The
//! binding between a declaration and the handler that serves it is unchecked**,
//! because every handler takes its params as an untyped JSON value and hand-parses
//! them — there is no type for the compiler to compare against `TAKES`. This is
//! the same shape of gap as any other pair of declarations in this chassis that
//! must move together with nothing in a trait able to see both. The repair a test
//! can make is narrower than the gap and is made on the producer side: an
//! integration test drives the live door and asserts a required param is actually
//! required.

use std::fmt;

/// **WHAT A SCALAR IS ON THE WIRE.** A payload leaf is JSON, and JSON offers
/// three scalar kinds a consumer can construct without being told anything else:
/// string, number, boolean. There are four members because the number splits.
///
/// `Int` and `Float` are apart because the operations are apart: `threads/list`'s
/// `limit` is a COUNT and `gauge/history`'s `days` is a MAGNITUDE. A consumer
/// that sent `100.5` as a count gets refused by serde with a message about a type
/// nothing ever told it, which is the diagnostic this split removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scalar {
    Str,
    Int,
    Float,
    Bool,
}

/// **HOW A TIMESTAMP IS SPELLED**, because one surface already serves both
/// spellings and nothing announced it.
///
/// `shapes/{shape}/subscribe`'s `at` is RFC3339; `accounts/health`'s `now_ms` is
/// epoch milliseconds. Two incompatible encodings of one dtype on one surface,
/// found by reading the code rather than by anything refusing a call — which is
/// why the encoding belongs in the declaration and not in a field name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimeEncoding {
    /// An RFC3339 string.
    Rfc3339,
    /// Integer milliseconds since the epoch.
    EpochMillis,
}

/// **WHAT UNIT A DURATION IS IN**, for the same reason and with the same
/// evidence: one API serves `days` (`gauge/history`), `expires_in_secs`
/// (`accounts/login/begin`) and `poll_interval_secs`, and today the unit lives
/// only in the spelling of the field. A name is not a type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimeUnit {
    Millis,
    Secs,
    Days,
}

/// **THE CLOSED SET — TWELVE MEMBERS.** Adding one is a finding about the model
/// before it is a change to this enum, and a thirteenth also has to be added to
/// [`FAMILIES`], which is where its lack of an instance becomes visible.
///
/// **The set is grounded in BOTH directions.** It was derived from what an
/// operation TAKES; since the cell manifest (`cell` in this crate) it also
/// describes what a shape CARRIES, and the producer-side census counts an
/// instance on either side. A member with no operation and no cell behind it is
/// still a claim about the future, and still fails that census.
///
/// Every variant's `Display` spelling reaches a consumer verbatim: `shapes.list`
/// publishes it as `CellInfo.dtype`. The spellings are therefore frozen, and
/// `the_inventory_spelling_of_a_dtype_is_frozen` in this file is what holds them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DType {
    /// A single JSON scalar. `threads/{thread_id}/send`'s `text` and `bare`,
    /// `threads/list`'s `limit`.
    Scalar(Scalar),
    /// **A CLOSED SET OF NAMED WORDS, LISTED.** Listing them is the entire
    /// difference between this and `Scalar(Str)`: a consumer can be checked
    /// against the list. `threads/{thread_id}/send`'s `intent`
    /// (`immediate | turn_end`), `gauge/history`'s `axis`, `budgets/{key}/set`'s
    /// `on_trip` (`kill | finish`).
    Enum(&'static [&'static str]),
    /// A nested object with a declared shape, named by its type.
    /// `threads/create`'s `settings` is `ThreadSettings`, 25 fields.
    Struct(&'static str),
    /// A homogeneous array. `threads/{thread_id}/history`'s `kinds`,
    /// `threads/{thread_id}/queue_reselect`'s `message_ids`.
    List(&'static DType),
    /// **AN ID IN THIS PRODUCER'S OWN ADDRESS SPACE**, naming what it addresses.
    /// Every path subject is one — `threads/{thread_id}`, `accounts/{account}`,
    /// `budgets/{key}` — as is `threads/{thread_id}/budget_assign`'s `key`.
    ///
    /// Separate from [`Self::Path`] because the producer can resolve one of them
    /// and cannot resolve the other, and a consumer has to know which before it
    /// constructs a value.
    Ref(&'static str),
    /// **A NAME IN THE HOST FILESYSTEM.**
    /// `threads/{thread_id}/reconcile_transcript`'s `path` — a `.jsonl` that must
    /// already exist on disk — plus `cwd`, `add_dirs`, `plugin_roots` and
    /// `plugin_dirs` inside the settings struct.
    ///
    /// It names no referent, deliberately: there is nothing inside this producer
    /// for it to refer to. That absence IS the declaration.
    Path,
    /// **A PATCH, NAMING WHAT IT PATCHES.** `threads/{thread_id}/update`'s
    /// `patch` is `Delta("ThreadSettings")`. The subtype is mandatory — the
    /// module doc has the reason, which is that a bare `delta` cannot express
    /// that `null` clears while absent leaves — and [`well_formed`] refuses an
    /// empty one at compile time.
    Delta(&'static str),
    /// **A PAYLOAD THE PRODUCER DELIBERATELY DOES NOT INTERPRET**, and the member
    /// the candidate set had no way to express.
    ///
    /// The operation that needs it is `threads/{thread_id}/event`, whose `data` is
    /// an arbitrary JSON value that lands verbatim in a custom event. This is not
    /// an undeclared corner: the shape design rules REQUIRE a general shape to
    /// hold an application's payloads opaquely, because the alternative is a
    /// general shape encoding one application's meaning.
    ///
    /// So this variant is a positive declaration that carries a PROHIBITION: the
    /// producer stores the value and returns it and may not branch on it. Calling
    /// it `Struct` would claim a shape nobody is allowed to rely on; leaving the
    /// operation undeclared would make the set describe only the easy half of the
    /// surface. `send`'s and `create`'s `metadata` are the same thing — stored
    /// and returned, never read.
    Opaque,
    /// **AN INSTANT, WITH ITS ENCODING.** `shapes/{shape}/subscribe`'s `at` is
    /// RFC3339 and validated (a past one is silently treated as absent);
    /// `gauge/history`'s `since` and `until` are RFC3339 and **never validated** —
    /// they go straight into a SQL comparison, so a malformed one quietly answers
    /// nothing instead of refusing. One dtype, two call sites, one of them
    /// checked: exactly the divergence a declared dtype exists to expose.
    ///
    /// Not a [`Self::Scalar`]: constructing one requires knowing the encoding,
    /// and `Str` does not carry it.
    Timestamp(TimeEncoding),
    /// **A LENGTH OF TIME, WITH ITS UNIT** — `gauge/history`'s `days`. See
    /// [`TimeUnit`] for why the unit is in the type and not in the field name.
    Duration(TimeUnit),
    /// **A POSITION IN AN ORDER, NAMING WHICH ORDER.**
    /// `threads/{thread_id}/history`'s `after_seq`, `threads/{thread_id}/query`'s
    /// `cursor`, `threads/{thread_id}/seen`'s `seq`,
    /// `threads/{thread_id}/queue_cancel`'s `message_id`.
    ///
    /// On the wire it is an integer, and it is still not a [`Self::Scalar`],
    /// because the rule for constructing one is different in kind: you do not
    /// COMPUTE a cursor, you echo back one the producer handed you.
    ///
    /// **This has already been paid for once.** An unread count is NOT
    /// `newest.seq - seen_seq`, because only addressable events count as unread;
    /// the producer-side test
    /// `the_unread_count_is_exact_where_the_position_difference_is_not` is the
    /// record of that bug. Subtracting two positions and calling the answer a
    /// quantity is the defect, and a dtype that spells positions apart from
    /// numbers is the repair.
    Cursor(&'static str),
    /// **A DICTIONARY FROM DYNAMIC KEYS TO ONE VALUE DTYPE** — the twelfth
    /// member, and the one the OUTPUT direction needed that the input direction
    /// never did.
    ///
    /// The first eleven came from operation payloads, where a leaf's context is
    /// its parent and a value never has to carry a key set of its own. A CELL is
    /// not a leaf: the chassis's own keyed write cell is a set of cells, and a
    /// shape carries one inside a row whenever a per-key fact has sub-keys of its
    /// own — thread id to placement, axis to reading, alias to host. Calling
    /// those `Struct` would claim a fixed field list they do not have; calling
    /// them `Opaque` would forbid reads the seam already makes into them. The
    /// inner dtype is the VALUE's; the keys are strings, as every JSON key is.
    ///
    /// No operation takes one, which is why it was not among the eleven. The
    /// census counts a cell declaration as an instance, and that is what grounds
    /// this member rather than a promise about some future call.
    Map(&'static DType),
}

impl DType {
    /// The family name — one per variant, so the producer-side census can name
    /// the member that has lost its last instance.
    pub const fn family(&self) -> &'static str {
        match self {
            DType::Scalar(_) => "scalar",
            DType::Enum(_) => "enum",
            DType::Struct(_) => "struct",
            DType::List(_) => "list",
            DType::Ref(_) => "ref",
            DType::Path => "path",
            DType::Delta(_) => "delta",
            DType::Opaque => "opaque",
            DType::Timestamp(_) => "timestamp",
            DType::Duration(_) => "duration",
            DType::Cursor(_) => "cursor",
            DType::Map(_) => "map",
        }
    }

    /// **THE DTYPE AT THE BOTTOM OF A CONTAINER CHAIN.** `list<map<struct:X>>`
    /// answers `struct:X`; anything that is not a container answers itself.
    /// `const`, because the cell manifest's compile-time checks read it.
    pub const fn leaf(&self) -> &DType {
        match self {
            DType::List(inner) | DType::Map(inner) => inner.leaf(),
            settled => settled,
        }
    }

    /// **THE CLOSED WORD LIST IF THE LEAF IS AN ENUM**, and empty otherwise.
    /// This is what a consumer may compare a value against, and what the cell
    /// manifest publishes so a consumer CAN be checked against it rather than
    /// being trusted to have read the docs.
    pub const fn words(&self) -> &'static [&'static str] {
        match self.leaf() {
            DType::Enum(words) => words,
            _ => &[],
        }
    }
}

/// **EVERY FAMILY IN THE CLOSED SET**, in one list.
///
/// Two jobs. The producer-side census iterates it to prove every member has at
/// least one declared instance — the mechanical form of *if a dtype has no
/// instance, do not add it*. And a thirteenth member has to be written here as
/// well as on the enum, which is the place where its absence of instances gets
/// noticed.
pub const FAMILIES: &[&str] = &[
    "scalar",
    "enum",
    "struct",
    "list",
    "ref",
    "path",
    "delta",
    "opaque",
    "timestamp",
    "duration",
    "cursor",
    "map",
];

/// **WHETHER A PARAMETER HAS TO BE THERE, AND WHAT ABSENCE MEANS.**
///
/// Three members, not two, because the surface already had three cases and two
/// of them were being spelled the same way. *Asking nothing means the ceiling* is
/// not the same statement as *this one is optional*: a consumer reading
/// `Optional` learns it may leave the field out, while a consumer reading
/// `Ceiling` learns that leaving it out is itself an ASK — for the most the
/// producer will serve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Presence {
    /// Absent is a refusal. `threads/{thread_id}/send`'s `text`.
    Required,
    /// Absent is legal and means the producer's own default.
    /// `threads/list`'s `limit` (100).
    Optional,
    /// **Absent means the CEILING.** `shapes/{shape}/subscribe`'s `detail`,
    /// `retain` and `beat`.
    Ceiling,
}

/// One declared parameter of an operation.
#[derive(Debug, Clone, Copy)]
pub struct Param {
    pub name: &'static str,
    pub dtype: DType,
    pub presence: Presence,
}

impl Param {
    pub const fn new(name: &'static str, dtype: DType, presence: Presence) -> Self {
        Self {
            name,
            dtype,
            presence,
        }
    }
}

/// **WHAT AN OPERATION DECLARES.** A path, and the payload it takes.
///
/// It does not declare what it ANSWERS, and that is settled rather than
/// unfinished. The answer has already been moved off the operation: four
/// commands stopped replying with rows, and what a write gets back is one
/// acceptance type shared by every write on the surface — not a per-operation
/// declaration. An operation that does still answer with state is answering with
/// a SHAPE's facts, and the shape is where that contract lives.
pub trait Operation {
    const PATH: &'static str;
    const TAKES: &'static [Param];
}

/// The census row a declaration contributes — what a producer's tests iterate
/// over. `declare_operation!` mints one per operation as `DECLARED`.
#[derive(Debug, Clone, Copy)]
pub struct Declared {
    pub path: &'static str,
    pub takes: &'static [Param],
}

/// **THE COMPILE-TIME CHECK.** A `const fn`, so `declare_operation!` can evaluate
/// it inside a `const` item and a malformed declaration fails the BUILD instead
/// of being discovered by a consumer.
///
/// Three rules, each named by the mistake it catches:
///
/// 1. **No empty subtype.** `Delta("")`, `Ref("")`, `Struct("")`, `Cursor("")` —
///    the subtype is the entire content of those variants, so an empty one says
///    nothing while looking like it says something. The check reaches through
///    `list` and `map`, because `list<ref:"">` is exactly as empty as the bare
///    one.
/// 2. **No empty enum, and no nameless parameter.** `Enum(&[])` is a closed set
///    with nothing in it: a parameter no value can satisfy.
/// 3. **No duplicate parameter name.** Two `Param`s under one name is a payload
///    with two meanings for one key — and JSON silently keeps the last, which is
///    the whole class of failure this chassis is built to catch at the door.
pub const fn well_formed(takes: &[Param]) -> bool {
    let mut i = 0;
    while i < takes.len() {
        if takes[i].name.is_empty() || !dtype_ok(&takes[i].dtype) {
            return false;
        }
        if name_repeats_after(takes, i) {
            return false;
        }
        i += 1;
    }
    true
}

/// Rule 3's scan: does `takes[i]`'s name appear again further along? Looking only
/// FORWARD is what keeps this linear-ish and means each colliding pair is found
/// exactly once.
const fn name_repeats_after(takes: &[Param], i: usize) -> bool {
    let mut j = i + 1;
    while j < takes.len() {
        if str_eq(takes[i].name, takes[j].name) {
            return true;
        }
        j += 1;
    }
    false
}

/// **ONE DTYPE IS WELL-FORMED** — the per-dtype half of [`well_formed`].
///
/// `pub` on purpose: the cell manifest macro (`declare_cells!`, exported from
/// this crate's `cell` module) asserts it once per cell and once per struct
/// field, with a message naming WHICH. A check that reports only *some dtype
/// around here is malformed* sets the reader a riddle.
pub const fn dtype_ok(d: &DType) -> bool {
    match d {
        DType::Struct(name) | DType::Ref(name) | DType::Delta(name) | DType::Cursor(name) => {
            !name.is_empty()
        }
        DType::Enum(words) => !words.is_empty(),
        DType::List(inner) | DType::Map(inner) => dtype_ok(inner),
        DType::Scalar(_) | DType::Timestamp(_) | DType::Duration(_) => true,
        DType::Path | DType::Opaque => true,
    }
}

/// `&str` equality in `const` context, which `==` is not. Bytes, because a
/// parameter name is ASCII and a length mismatch settles it first.
const fn str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// **THE SPELLING A CONSUMER READS.** `shapes.list` publishes this string as
/// `CellInfo.dtype`, so every byte of it is part of the protocol: `scalar:Int`,
/// `enum["kill", "finish"]`, `struct:ThreadSettings`, `list<struct:MoveItem>`,
/// `map<opaque>`, `timestamp:Rfc3339`.
///
/// The parts a consumer actually CHECKS against are broken out beside it in the
/// frame — the closed word list and the one-level field list — so that nobody has
/// to parse this string to use it.
impl fmt::Display for DType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DType::Scalar(s) => write!(f, "scalar:{s:?}"),
            DType::Enum(v) => write!(f, "enum{v:?}"),
            DType::Struct(n) => write!(f, "struct:{n}"),
            DType::List(i) => write!(f, "list<{i}>"),
            DType::Ref(n) => write!(f, "ref:{n}"),
            DType::Path => write!(f, "path"),
            DType::Delta(n) => write!(f, "delta:{n}"),
            DType::Opaque => write!(f, "opaque"),
            DType::Timestamp(e) => write!(f, "timestamp:{e:?}"),
            DType::Duration(u) => write!(f, "duration:{u:?}"),
            DType::Cursor(n) => write!(f, "cursor:{n}"),
            DType::Map(i) => write!(f, "map<{i}>"),
        }
    }
}

/// **DECLARE ONE OPERATION, WITH THE CHECK ATTACHED.**
///
/// The macro exists so that the `const` assertion cannot be forgotten: there is
/// no way to declare an operation through this door without [`well_formed`] being
/// evaluated over its payload at compile time. A hand-written `Operation` impl
/// skips the check — the same unchecked-pairing gap this chassis has elsewhere,
/// and the reason the macro is the documented way in.
///
/// It emits three things: the marker type, its `Operation` impl, and a
/// `DECLARED` const that the producer's census iterates.
///
/// ```ignore
/// declare_operation!(
///     pub Update,
///     "threads/{thread_id}/update",
///     [Param::new("patch", DType::Delta("ThreadSettings"), Presence::Required)]
/// );
/// ```
#[macro_export]
macro_rules! declare_operation {
    ($vis:vis $name:ident, $path:literal, [$($p:expr),* $(,)?]) => {
        $vis struct $name;
        impl $crate::operation::Operation for $name {
            const PATH: &'static str = $path;
            const TAKES: &'static [$crate::operation::Param] = &[$($p),*];
        }
        impl $name {
            $vis const DECLARED: $crate::operation::Declared = $crate::operation::Declared {
                path: $path,
                takes: &[$($p),*],
            };
        }
        // THE CHECK ITSELF. A malformed payload is a build failure in the
        // PRODUCER's crate, which is what "the check runs at compile time" means
        // for this surface.
        //
        // The message is a plain literal and must stay one. `assert!` reads its
        // second argument as a FORMAT string, so a `concat!` that splices the
        // path in turns a route like `threads/{thread_id}/send` into a capture of
        // a nonexistent argument and the build then fails for the wrong reason,
        // naming the wrong defect. The assertion's span already points at the
        // offending declaration, so the path adds nothing.
        const _: () = assert!(
            $crate::operation::well_formed(&[$($p),*]),
            "malformed operation payload declaration: an empty subtype, an empty \
             enum, a nameless parameter, or two parameters of one name",
        );
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **THE CHECKER IS EVALUATED IN `const` CONTEXT, WHICH IS THE WHOLE CLAIM.**
    ///
    /// If [`well_formed`] were an ordinary `fn` this item would not compile. Its
    /// presence is therefore the evidence that `declare_operation!`'s assertion
    /// is a BUILD failure and not a runtime one — the property no `#[test]` can
    /// state, because a test that runs has already got past the compiler.
    const _PROOF: bool = well_formed(&[Param::new(
        "x",
        DType::Scalar(Scalar::Str),
        Presence::Required,
    )]);

    /// One instance of each of the twelve families, as a (family, dtype) pair.
    /// Shared by the two tests below so they cannot disagree about what the set
    /// contains.
    const ONE_OF_EACH: &[(&str, DType)] = &[
        ("scalar", DType::Scalar(Scalar::Int)),
        ("enum", DType::Enum(&["kill", "finish"])),
        ("struct", DType::Struct("ThreadSettings")),
        ("list", DType::List(&DType::Scalar(Scalar::Str))),
        ("ref", DType::Ref("thread")),
        ("path", DType::Path),
        ("delta", DType::Delta("ThreadSettings")),
        ("opaque", DType::Opaque),
        ("timestamp", DType::Timestamp(TimeEncoding::Rfc3339)),
        ("duration", DType::Duration(TimeUnit::Days)),
        ("cursor", DType::Cursor("seq")),
        ("map", DType::Map(&DType::Opaque)),
    ];

    /// **A `delta` WITH NO NAMED SUBTYPE IS REFUSED, AND A NAMED ONE SHIPS.**
    ///
    /// A bare `delta` cannot say that `null` clears and absent leaves, which is
    /// the one thing a consumer must know before it sends one. The third
    /// assertion is the half a shallow check would pass: `list<ref:"">` hides an
    /// empty subtype one level down.
    ///
    /// **FORCED RED BY:** moving `DType::Delta(name)` out of the non-empty-name
    /// arm of `dtype_ok` and into the always-true arm.
    /// **OPPOSED BY:** the positive assertion — an implementation that refuses
    /// everything fails this test too.
    #[test]
    fn a_delta_with_no_named_subtype_is_refused() {
        assert!(
            !well_formed(&[Param::new("patch", DType::Delta(""), Presence::Required)]),
            "a `delta` with no named subtype is refused at declaration"
        );
        assert!(
            well_formed(&[Param::new(
                "patch",
                DType::Delta("ThreadSettings"),
                Presence::Required
            )]),
            "...and the named one is the declaration that actually ships"
        );
        assert!(
            !well_formed(&[Param::new(
                "ks",
                DType::List(&DType::Ref("")),
                Presence::Optional
            )]),
            "the check reaches INSIDE a container: `list<ref:\"\">` is as empty \
             as a bare one, and a shallow check would wave it through"
        );
    }

    /// **TWO PARAMETERS OF ONE NAME ARE REFUSED**, along with a nameless one and
    /// an enum with no words.
    ///
    /// One key with two meanings is a payload where JSON quietly keeps the last
    /// value — the collapse class this chassis exists to stop at the door rather
    /// than debug in a transcript.
    ///
    /// **FORCED RED BY:** making `name_repeats_after` return `false`
    /// unconditionally (equivalently, deleting its `while` loop).
    /// **OPPOSED BY:** the final assertion, which accepts two DIFFERENT names —
    /// so a checker that refuses every payload cannot pass this test either.
    /// substrate's version of this test had only refusals and was passed by a
    /// constantly-false checker.
    #[test]
    fn two_parameters_of_one_name_are_refused() {
        assert!(
            !well_formed(&[
                Param::new("limit", DType::Scalar(Scalar::Int), Presence::Optional),
                Param::new("limit", DType::Scalar(Scalar::Str), Presence::Optional),
            ]),
            "two parameters of one name are refused at declaration"
        );
        assert!(
            !well_formed(&[Param::new("", DType::Opaque, Presence::Optional)]),
            "...and so is a nameless parameter"
        );
        assert!(
            !well_formed(&[Param::new("k", DType::Enum(&[]), Presence::Required)]),
            "an `enum` with no words is a parameter nothing can satisfy"
        );
        assert!(
            well_formed(&[
                Param::new("limit", DType::Scalar(Scalar::Int), Presence::Optional),
                Param::new("after_seq", DType::Cursor("seq"), Presence::Optional),
            ]),
            "two parameters of two names are a payload, not a collision"
        );
    }

    /// **THE INVENTORY SPELLING OF EVERY DTYPE IS FROZEN.** `shapes.list` ships
    /// `Display` output as `CellInfo.dtype`, so these strings are protocol, not
    /// debug output: a consumer and a generated inventory both read them. Twelve
    /// distinct expected strings mean no constant implementation passes.
    ///
    /// The nesting case is the one that is easy to get wrong twice, since `leaf`
    /// and `Display` recurse independently.
    ///
    /// **FORCED RED BY:** changing the `Struct` arm's format string from
    /// `"struct:{n}"` to `"struct {n}"`.
    #[test]
    fn the_inventory_spelling_of_a_dtype_is_frozen() {
        let expected = [
            "scalar:Int",
            r#"enum["kill", "finish"]"#,
            "struct:ThreadSettings",
            "list<scalar:Str>",
            "ref:thread",
            "path",
            "delta:ThreadSettings",
            "opaque",
            "timestamp:Rfc3339",
            "duration:Days",
            "cursor:seq",
            "map<opaque>",
        ];
        assert_eq!(
            ONE_OF_EACH.len(),
            expected.len(),
            "one expected spelling per family"
        );
        for ((family, dtype), want) in ONE_OF_EACH.iter().zip(expected) {
            assert_eq!(
                dtype.to_string(),
                want,
                "the published spelling of `{family}` is frozen"
            );
        }

        // Containers nest, and `Display` has to recurse the whole way down.
        assert_eq!(
            DType::List(&DType::Map(&DType::Struct("MoveItem"))).to_string(),
            "list<map<struct:MoveItem>>"
        );
        // The other two encodings and the other two units, since each one is a
        // separate `Debug` name on the wire.
        assert_eq!(
            DType::Timestamp(TimeEncoding::EpochMillis).to_string(),
            "timestamp:EpochMillis"
        );
        assert_eq!(
            DType::Duration(TimeUnit::Millis).to_string(),
            "duration:Millis"
        );
        assert_eq!(DType::Duration(TimeUnit::Secs).to_string(), "duration:Secs");
    }

    /// **THE CENSUS LIST AND THE ENUM AGREE, AND THE LEAF OF A CHAIN IS ITS
    /// BOTTOM.**
    ///
    /// [`FAMILIES`] is the list the producer-side census iterates to find a
    /// member with no declared instance. That census needs declared operations
    /// and so lives in the producer's crate; what CAN be held here is the thing
    /// it silently depends on — that the list and the variants are the same
    /// twelve. A family added to one and not the other makes the census lie in
    /// whichever direction it was skipped.
    ///
    /// **FORCED RED BY:** adding `"blob"` to `FAMILIES` — the candidate set's
    /// seventh member, restored with nothing behind it. (That is also the exact
    /// mutation named by the producer-side census test.)
    /// **OPPOSED BY:** the per-family membership assertion, which fails if
    /// `FAMILIES` is emptied or a `family()` arm is renamed.
    #[test]
    fn the_census_list_and_the_variants_are_the_same_twelve() {
        assert_eq!(FAMILIES.len(), 12, "the closed set has twelve members");
        assert_eq!(ONE_OF_EACH.len(), 12, "one instance per member");

        for (family, dtype) in ONE_OF_EACH {
            assert_eq!(
                dtype.family(),
                *family,
                "the instance declared for `{family}` is of that family"
            );
            assert!(
                FAMILIES.contains(family),
                "`{family}` is a variant of the enum and must be in the census \
                 list, or the census cannot see it"
            );
        }
        for listed in FAMILIES {
            assert!(
                ONE_OF_EACH.iter().any(|(f, _)| f == listed),
                "`{listed}` is in the census list with no variant behind it"
            );
        }
    }

    // A real declaration through the documented door, so that the macro's
    // expansion is exercised by something other than a producer crate that does
    // not exist yet in this tree.
    declare_operation!(
        Update,
        "threads/{thread_id}/update",
        [Param::new(
            "patch",
            DType::Delta("ThreadSettings"),
            Presence::Required
        )]
    );

    /// **THE MACRO'S THREE OUTPUTS DESCRIBE ONE OPERATION.** `declare_operation!`
    /// expands the payload list TWICE — once for `Operation::TAKES` and once for
    /// the `DECLARED` census row — and nothing but this test says the two
    /// expansions are the same declaration. The path is spelled once and lands in
    /// two places, with the same exposure.
    ///
    /// The route string is also protocol: a consumer dials what `PATH` says.
    ///
    /// **FORCED RED BY:** changing the macro's `DECLARED` initialiser from
    /// `path: $path` to `path: "threads/{thread_id}"`.
    /// **OPPOSED BY:** the `TAKES` assertions, which pin the other expansion, so
    /// a macro that emitted an empty payload for both would still fail.
    #[test]
    fn the_declared_census_row_matches_the_operation_it_came_from() {
        assert_eq!(Update::PATH, "threads/{thread_id}/update");
        assert_eq!(Update::DECLARED.path, Update::PATH);
        assert_eq!(Update::TAKES.len(), 1);
        assert_eq!(Update::DECLARED.takes.len(), Update::TAKES.len());
        assert_eq!(Update::TAKES[0].name, "patch");
        assert_eq!(Update::TAKES[0].dtype.to_string(), "delta:ThreadSettings");
        assert!(matches!(Update::TAKES[0].presence, Presence::Required));
        assert_eq!(Update::DECLARED.takes[0].name, Update::TAKES[0].name);
    }

    /// **A CONTAINER CHAIN ANSWERS WITH ITS BOTTOM, AND THE WORD LIST COMES FROM
    /// THERE.** `words()` is what the cell manifest publishes so a consumer can
    /// be checked against a closed vocabulary; it reads through `list` and `map`,
    /// because a `list<enum[...]>` constrains its elements just as the bare enum
    /// does.
    ///
    /// **FORCED RED BY:** replacing `leaf`'s container arm with `settled =>
    /// settled` for all inputs (i.e. making it never recurse).
    /// **OPPOSED BY:** the non-container case, which an always-recursing or
    /// always-empty implementation gets wrong.
    #[test]
    fn the_leaf_of_a_container_chain_carries_the_word_list() {
        let bare = DType::Enum(&["immediate", "turn_end"]);
        let wrapped = DType::List(&DType::Map(&DType::Enum(&["immediate", "turn_end"])));

        assert_eq!(bare.words(), &["immediate", "turn_end"]);
        assert_eq!(
            wrapped.words(),
            &["immediate", "turn_end"],
            "a `list<map<enum>>` constrains its elements, so the words have to \
             survive the unwrapping"
        );
        assert_eq!(wrapped.leaf().family(), "enum");
        assert_eq!(wrapped.family(), "list", "the outer family is still `list`");

        // And nothing else has words — an empty list here is the statement *this
        // value is not compared against a vocabulary*, not a missing one.
        assert!(DType::Scalar(Scalar::Str).words().is_empty());
        assert!(DType::Struct("ThreadSettings").words().is_empty());
        assert!(
            DType::List(&DType::Scalar(Scalar::Int)).words().is_empty(),
            "a `list<scalar>` has no vocabulary, and the leaf walk must not \
             invent one"
        );
    }
}
