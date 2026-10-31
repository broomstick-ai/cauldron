//! # The cell manifest — what a shape CARRIES, declared
//!
//! `operation` is this module's input twin. There, `declare_operation!` buys a
//! producer compile-time well-formedness for what a leaf TAKES. Here the same
//! construction buys it for what a shape EMITS, which before this module nobody
//! declared at all.
//!
//! ## The hole, in three layers
//!
//! A consumer reads a value out of a row by naming three things, and only the
//! outermost of them was ever written down:
//!
//! | layer | example | declared before this? | what a mirror answered when it was not |
//! |---|---|---|---|
//! | shape | `threads` | yes — it is a row of `shapes.list` | *not in this face's book* |
//! | cell | `status` | **no** — `ShapeInfo` published `keys`, never a cell list | *unseen* — **the same answer as "has not arrived yet"** |
//! | word | `state == "pending"` | **no** — the closed vocabulary lived in a doc comment | `None`, then `.unwrap_or(false)`, then silence |
//!
//! That middle row is not hypothetical. A face read a `pending` field off
//! `threads` rows for months. No producer ever carried it. The mirror answered
//! *not yet* for the entire life of every process that asked, the face read *not
//! yet* as *nothing is pending*, and a veto built on it never fired once. The
//! three-valued cell state was honest about DELIVERY and structurally could not
//! be honest about EXISTENCE, because the producer had never said which cells
//! exist. Nobody wrote a bug; everybody wrote code against a cell that was never
//! coming.
//!
//! ## Where the promise is collected
//!
//! A promise becomes enforceable at the moment it becomes somebody else's. For a
//! shape that moment is registration: a registered shape is one consumers may
//! read off, so registration is where a cell list is owed. `Shape::CELLS`
//! (per-key) and `Shape::SHARED` (shape-level) are therefore REQUIRED trait
//! consts with **no default**, because no default is true. An empty list is not
//! what any shape carries, and a chassis that defaulted it would publish a
//! manifest nobody wrote — this module's own hole in a new coat. A shape that
//! declares neither does not build, and rustc names what is missing:
//!
//! ```text
//! error[E0046]: not all trait items implemented, missing: `CELLS`, `SHARED`
//! ```
//!
//! Three doors, each shut by a different mechanism:
//!
//! 1. **[`Cells`] is sealed by the TYPE.** Its one field is private and its only
//!    constructor is the one [`declare_cells!`] expands to, so a manifest cannot
//!    exist without having gone through the macro — and the macro plants one
//!    `const` assertion per rule per cell beside the declaration.
//!    `declare_operation!` documents the gap that a hand-written impl skips its
//!    check; here the gap is closed by making the hand-written form
//!    unrepresentable.
//! 2. **The manifest rides `shapes.list`** as `ShapeInfo::cells` and
//!    `ShapeInfo::shared_cells` — required there too, never serde-defaulted. A
//!    defaulted empty manifest is a consumer concluding *this producer carries
//!    nothing*, which is a positive claim serde would have invented.
//! 3. **The mirror learns it** and can finally answer the fourth thing: asked
//!    about a name the producer never declared, `Mirror::declared` says *there is
//!    no such cell*, which is a different answer from *not yet* for a name it
//!    did.
//!
//! The seam reference kit routes reads against this inventory exactly as it
//! routes methods: every cell a `view::*` function names, and every word it
//! compares a value against, has to be one some producer declares. `pending` is
//! the name that fails there.
//!
//! ## One-directional, on purpose
//!
//! A consumer may read only what is declared. A producer may send MORE than it
//! declares. That asymmetry is what keeps rule 2.6's cheapening property intact
//! — additive changes need no version bump, provided the mirror drops what it
//! does not recognise — and the mirror still drops an undeclared cell that
//! arrives. The only thing that became impossible is building a read of a cell
//! that will NEVER arrive and having it look like patience.
//!
//! `deny_unknown_fields` is the obvious-looking alternative and it is refused: it
//! bites in both directions, so it breaks the cheapening property while saying
//! nothing at all about the one case that matters here — the field the consumer
//! expects and the producer never sends.
//!
//! ## How deep a declaration goes
//!
//! A [`Cell`] is a name, a `DType`, a `Presence`, and — when the dtype's leaf is
//! a `Struct` — the fields that struct carries, each a `Param`, declared exactly
//! as an operation's payload is. Fields go ONE level deep: a field whose own
//! dtype is `Struct("X")` names `X` and stops. That is the depth the seam reads
//! at, and anything deeper would be describing values no checker looks at. A cell
//! whose contents nobody may read is declared `DType::Opaque` — a positive
//! declaration that carries a prohibition, not an omission.
//!
//! **A tagged union is declared as its tag plus the union of its fields, all
//! `Optional`.** `narration`'s lines are `kind: progress|lifecycle|info` with
//! fields that depend on the kind; the declaration gives the tag's words and
//! lists every field any variant carries, optional. That is weaker than the
//! truth, and saying so is part of the contract: this manifest cannot tell a
//! consumer which fields go with which kind, only that each named field is one
//! the producer may send.
//!
//! ## What is NOT checked here
//!
//! Named, so the gap is not mistaken for coverage.
//!
//! **That a shape's row builder emits exactly the manifest.** The chassis cannot
//! see inside a producer's `Root::cells` any more than it can pair a retention
//! ceiling with the function that trims. That is a test's job, one per producer:
//! build every shape's cells block and assert its key set equals the manifest's.
//! That test is the positive sentinel for the declaration — a manifest that
//! drifts from the row goes red in the producer's own suite, where the person who
//! moved the row is looking.
//!
//! **That a declared `Required` field is present on every row.** That is runtime
//! honesty and the consuming half carries it: a read of a missing required field
//! lands as a not-hydrated answer naming the field, never as a defaulted value.

use crate::operation::{DType, Param, Presence};
use crate::wire::{CellInfo, FieldInfo};

/// **ONE DECLARED CELL.** The name a row spells it with, what the value is,
/// whether it may be null, and — for a struct — what it carries.
///
/// Built only by [`declare_cells!`]. The fields are `pub` because the macro
/// expands in the producer's crate and has to name them; that is not a second
/// door, because a hand-built `Cell` has nowhere to go — [`Cells`] is the type
/// every consumer of a manifest is written against and its constructor is the
/// macro's.
#[derive(Debug, Clone, Copy)]
pub struct Cell {
    pub name: &'static str,
    pub dtype: DType,
    /// `Required`: once the cell is known the row always carries a non-null
    /// value. `Optional`: it may be null or absent, and that absence is itself a
    /// statement (*not placed*, *no title*) rather than a defect.
    pub presence: Presence,
    /// The fields of the dtype's `Struct` leaf, one level deep. Empty for every
    /// other leaf and never empty for a `Struct`; [`fields_fit`] refuses both
    /// mismatches at compile time.
    pub fields: &'static [Param],
}

impl Cell {
    /// The wire form, for `shapes.list`.
    pub fn publish(&self) -> CellInfo {
        CellInfo {
            name: self.name.to_string(),
            dtype: self.dtype.to_string(),
            required: is_required(self.presence),
            words: words_of(&self.dtype),
            fields: self.fields.iter().map(publish_field).collect(),
        }
    }
}

/// One declared field, in wire form. A field is a cell minus the field list,
/// because the declaration stops one level down.
fn publish_field(f: &Param) -> FieldInfo {
    FieldInfo {
        name: f.name.to_string(),
        dtype: f.dtype.to_string(),
        required: is_required(f.presence),
        words: words_of(&f.dtype),
    }
}

/// The wire spells presence as a bool, which is lossless here only because
/// [`presence_ok`] has already refused the third variant at compile time.
fn is_required(p: Presence) -> bool {
    matches!(p, Presence::Required)
}

/// The closed vocabulary at the dtype's leaf, owned. Empty for anything that is
/// not an enum — and an empty `words` is dropped on serialisation, so *no words*
/// and *no field named `words`* are the same statement from both ends.
fn words_of(d: &DType) -> Vec<String> {
    d.words().iter().map(|w| w.to_string()).collect()
}

/// **A SHAPE'S CELL MANIFEST, SEALED.**
///
/// One private field, no public constructor: the only way to hold one is to have
/// written a [`declare_cells!`] block, which plants the compile-time checks
/// beside it. `Shape::CELLS` and `Shape::SHARED` are typed on this rather than on
/// `&[Cell]` for exactly that reason — a bare slice can be written by hand, and a
/// hand-written manifest skips every check.
#[derive(Debug, Clone, Copy)]
pub struct Cells {
    cells: &'static [Cell],
}

impl Cells {
    /// **A SHAPE WITH NOTHING AT THIS LEVEL** — the honest value for `SHARED` on
    /// a shape that has no shape-level cell, stated rather than defaulted.
    /// Writing this is a declaration: *there is nothing here*. A consumer reading
    /// `[]` off `shapes.list` is reading that statement, not the absence of one.
    pub const NONE: Cells = Cells { cells: &[] };

    /// **THE MACRO'S CONSTRUCTOR, NOT A DOOR.** The checks a manifest owes are
    /// emitted by [`declare_cells!`] beside the call to this, as `const` items
    /// the compiler evaluates whether or not the manifest is ever read. Calling
    /// this directly skips all of them, which is why it is hidden and
    /// double-underscored instead of documented.
    #[doc(hidden)]
    pub const fn __declared(cells: &'static [Cell]) -> Cells {
        Cells { cells }
    }

    pub const fn cells(&self) -> &'static [Cell] {
        self.cells
    }

    pub const fn len(&self) -> usize {
        self.cells.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// The declared cell of this name, if the manifest has one.
    pub fn get(&self, name: &str) -> Option<&'static Cell> {
        self.cells.iter().find(|c| c.name == name)
    }

    /// Every declared name, in declaration order.
    pub fn names(&self) -> Vec<&'static str> {
        self.cells.iter().map(|c| c.name).collect()
    }

    /// **EVERY `Ref` NAME THIS MANIFEST USES**, taken from a cell's leaf or a
    /// field's, each once, in first-seen order.
    ///
    /// Registration checks a shape's declared referents against this list: a
    /// referent named for a reference no cell carries is a declaration about
    /// nothing, and is refused there.
    pub fn ref_names(&self) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        for cell in self.cells {
            let dtypes = std::iter::once(&cell.dtype).chain(cell.fields.iter().map(|f| &f.dtype));
            for dtype in dtypes {
                if let DType::Ref(name) = dtype.leaf() {
                    if !out.contains(name) {
                        out.push(name);
                    }
                }
            }
        }
        out
    }

    /// The wire form, for `shapes.list`.
    pub fn publish(&self) -> Vec<CellInfo> {
        self.cells.iter().map(Cell::publish).collect()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// THE CHECKS
//
// Every one of them is a `const fn`, which is the whole point: `declare_cells!`
// asserts each in a `const` item, so a malformed manifest is a BUILD failure at
// the declaration rather than a surprise at registration. `const` is also why
// these are `while` loops over indices and not iterators — a `const fn` has no
// `Iterator`, and no string comparison either, which is what `same_str` is for.
// ─────────────────────────────────────────────────────────────────────────────

/// **`name` IS DECLARED EXACTLY ONCE.** Two cells of one name is a row with two
/// meanings for one slot, and JSON keeps the last — so the second declaration
/// describes nothing a consumer could ever read.
pub const fn named_once(cells: &[Cell], name: &str) -> bool {
    count_cells(cells, name) == 1
}

/// **AN EMITTED THING HAS NO CEILING.** `Presence::Ceiling` is rule 2.8's *asking
/// nothing means the ceiling*, which is a statement about a parameter a
/// SUBSCRIBER sets. A cell or a field is emitted, so it is `Required` (never
/// null) or `Optional` (may be null or absent), and there is no third thing it
/// could mean.
pub const fn presence_ok(p: Presence) -> bool {
    !matches!(p, Presence::Ceiling)
}

/// **A `Struct` LEAF LISTS ITS FIELDS; EVERY OTHER LEAF LISTS NONE.**
///
/// Both directions, because each is a declaration that contradicts itself. A
/// struct with no fields listed cannot be checked against at all — declare the
/// cell `Opaque` if nothing may read inside it. A scalar with fields is a dtype
/// and a field list describing two different values.
///
/// The check reads the dtype's LEAF, so it reaches through `list<…>` and
/// `map<…>`: a list of structs owes its fields the same as a bare struct does.
pub const fn fields_fit(dtype: &DType, fields: &[Param]) -> bool {
    match dtype.leaf() {
        DType::Struct(_) => !fields.is_empty(),
        _ => fields.is_empty(),
    }
}

/// **FIELD `field` OF CELL `cell` IS DECLARED EXACTLY ONCE.**
///
/// The cell is looked up by name, which is well defined because [`named_once`]
/// is asserted beside this on the same manifest. A cell the manifest does not
/// declare answers `false`, so a check written against a misspelled cell name
/// cannot pass by finding nothing to count.
pub const fn field_named_once(cells: &[Cell], cell: &str, field: &str) -> bool {
    match find_cell(cells, cell) {
        Some(c) => count_fields(c.fields, field) == 1,
        None => false,
    }
}

/// How many cells carry this name. Factored out because the duplicate-name rule
/// and the lookup both want it, and a second hand-written loop is a second place
/// for the comparison to go wrong.
const fn count_cells(cells: &[Cell], name: &str) -> usize {
    let mut found = 0;
    let mut i = 0;
    while i < cells.len() {
        if same_str(cells[i].name, name) {
            found += 1;
        }
        i += 1;
    }
    found
}

/// How many fields carry this name, within one cell's field list.
const fn count_fields(fields: &[Param], name: &str) -> usize {
    let mut found = 0;
    let mut i = 0;
    while i < fields.len() {
        if same_str(fields[i].name, name) {
            found += 1;
        }
        i += 1;
    }
    found
}

/// The first cell of this name, or `None`. First rather than only: the manifest
/// may legally be malformed at the instant this runs, since the duplicate check
/// is a sibling assertion and not a precondition.
const fn find_cell<'m>(cells: &'m [Cell], name: &str) -> Option<&'m Cell> {
    let mut i = 0;
    while i < cells.len() {
        if same_str(cells[i].name, name) {
            return Some(&cells[i]);
        }
        i += 1;
    }
    None
}

/// `str` equality in a `const fn`. Neither `==` on slices nor `str::eq` is
/// `const` on stable, so this is a byte loop and will stay one until they are.
const fn same_str(a: &str, b: &str) -> bool {
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

/// **DECLARE A SHAPE'S CELLS, WITH THE CHECKS.**
///
/// ```ignore
/// shapewire::declare_cells! {
///     /// Per-key cells of the `threads` shape.
///     pub THREADS_CELLS = [
///         ("status", Struct("StateLevel"), Required, [
///             ("state", Enum(&["active", "queued", "parked", "retrying", "idle"]), Required),
///             ("executing", Scalar(Bool), Required),
///             ("detail", Scalar(Str), Optional),
///         ]),
///         ("queued", Scalar(Int), Required, []),
///         ("account", Ref("account"), Optional, []),
///     ]
/// }
///
/// impl Shape<Input> for Threads {
///     const CELLS: Cells = THREADS_CELLS;
///     const SHARED: Cells = THREADS_SHARED;
///     // …
/// }
/// ```
///
/// A cell is `(name, dtype, presence, [fields])` and a field is `(name, dtype,
/// presence)`. **The names are LITERALS, not expressions**, and that is the one
/// constraint the syntax imposes for the checks' sake: every assertion the macro
/// emits has to be able to say which cell and which field it is about, and a
/// check that cannot name what is wrong is a riddle with a build break attached.
///
/// The checks are `const` items planted beside the manifest, evaluated whether or
/// not anything reads it, so `cargo check` and an IDE both see them. A malformed
/// manifest fails at the declaration:
///
/// ```text
/// error[E0080]: evaluation panicked: cell `status` is declared more than once
///               in this manifest. One name, one cell: a row has one slot per
///               name and JSON keeps the last, so the second declaration
///               describes nothing a consumer could read
///   --> services/threads/src/shapes/threads.rs:NNN:1
/// ```
///
/// There is no spelling of a [`Cells`] that skips them. The type is sealed and
/// this macro is its only door.
#[macro_export]
macro_rules! declare_cells {
    (
        $(#[$meta:meta])*
        $vis:vis $name:ident = [
            $( (
                $cell:literal , $dtype:expr , $presence:expr ,
                [ $( ( $fname:literal , $fdtype:expr , $fpresence:expr ) ),* $(,)? ]
            ) ),* $(,)?
        ]
    ) => {
        $(#[$meta])*
        $vis const $name: $crate::cell::Cells = $crate::cell::Cells::__declared(&[
            $( $crate::cell::Cell {
                name: $cell,
                dtype: $dtype,
                presence: $presence,
                fields: &[ $( $crate::operation::Param::new($fname, $fdtype, $fpresence) ),* ],
            } ),*
        ]);
        // ONE `const` PER RULE PER CELL, each carrying a literal message that
        // names the cell — and the field, where there is one. A span says WHERE
        // and never WHAT, and the whole value of planting these at the
        // declaration is that the producer reads a sentence instead of a line
        // number.
        //
        // The messages are built with `concat!` over literals because `assert!`
        // treats its message as a FORMAT STRING: a runtime `{}` in there would
        // be a second, worse failure on top of the first. That is safe here
        // because a cell name never contains a brace, which is the only thing
        // that would break it.
        $(
            const _: () = assert!(
                !$cell.is_empty(),
                "a cell with no name is declared in this manifest; every cell is read by its name, so a nameless one can never be read",
            );
            const _: () = assert!(
                $crate::cell::named_once($name.cells(), $cell),
                concat!("cell `", $cell, "` is declared more than once in this manifest. One name, one cell: a row has one slot per name and JSON keeps the last, so the second declaration describes nothing a consumer could read"),
            );
            const _: () = assert!(
                $crate::operation::dtype_ok(&$dtype),
                concat!("cell `", $cell, "`: its dtype has an empty subtype or an empty enum — a declaration that says nothing while looking like it says something. Name the struct, ref, delta or cursor; list the enum's words"),
            );
            const _: () = assert!(
                $crate::cell::presence_ok($presence),
                concat!("cell `", $cell, "` is declared `Ceiling`. A ceiling belongs to a parameter a SUBSCRIBER sets (rule 2.8); a cell is EMITTED, so it is `Required` (never null once known) or `Optional` (may be null or absent)"),
            );
            const _: () = assert!(
                $crate::cell::fields_fit(&$dtype, &[ $( $crate::operation::Param::new($fname, $fdtype, $fpresence) ),* ]),
                concat!("cell `", $cell, "`: a `Struct` leaf lists its fields and any other leaf lists none. A struct with no fields is a declaration no consumer can be checked against — list what it carries, or declare the cell `Opaque`, which is the positive declaration that nothing may read inside it"),
            );
            $(
                const _: () = assert!(
                    !$fname.is_empty(),
                    concat!("cell `", $cell, "` declares a field with no name; a field is read by its name"),
                );
                const _: () = assert!(
                    $crate::cell::field_named_once($name.cells(), $cell, $fname),
                    concat!("cell `", $cell, "`, field `", $fname, "` is declared more than once; one name, one field"),
                );
                const _: () = assert!(
                    $crate::operation::dtype_ok(&$fdtype),
                    concat!("cell `", $cell, "`, field `", $fname, "`: its dtype has an empty subtype or an empty enum — name the struct, ref, delta or cursor; list the enum's words"),
                );
                const _: () = assert!(
                    $crate::cell::presence_ok($fpresence),
                    concat!("cell `", $cell, "`, field `", $fname, "` is declared `Ceiling`; an emitted field is `Required` or `Optional`"),
                );
            )*
        )*
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::{
        DType::{Enum, List, Map, Opaque, Ref, Scalar as Sc, Struct},
        Presence::{Ceiling, Optional, Required},
        Scalar::{Bool, Int, Str},
    };

    /// **THE CHECKERS EVALUATE IN `const` CONTEXT, WHICH IS THE WHOLE CLAIM.**
    ///
    /// This item is the evidence, not an assertion: if any one of the four were
    /// an ordinary `fn`, this `const` would not compile, and `declare_cells!`'s
    /// assertions would be runtime panics at registration instead of build
    /// failures at the declaration. `the_const_proof_holds` reads the values back
    /// so the item is live rather than dead.
    const PROOF: (bool, bool, bool, bool) = {
        const C: &[Cell] = &[Cell {
            name: "x",
            dtype: Sc(Int),
            presence: Required,
            fields: &[],
        }];
        (
            named_once(C, "x"),
            presence_ok(Required),
            fields_fit(&Sc(Int), &[]),
            field_named_once(C, "x", "y"),
        )
    };

    // **THE MACRO'S OWN DOOR, ON A WELL-FORMED MANIFEST.** The positive control
    // for every refusal below: a gate that refuses everything is not a gate, and
    // this block failing to compile is the forcing that would catch one.
    //
    // `app` and `moves` are here to carry the container cases — the fields check
    // and the enum words both have to reach through a `map<…>` and a `list<…>` to
    // the leaf. `account` carries the `Ref` case.
    crate::declare_cells! {
        WELL_FORMED = [
            ("status", Struct("StateLevel"), Required, [
                ("state", Enum(&["active", "idle"]), Required),
                ("executing", Sc(Bool), Required),
                ("detail", Sc(Str), Optional),
            ]),
            ("queued", Sc(Int), Required, []),
            ("app", Map(&Opaque), Required, []),
            ("moves", List(&Struct("MoveItem")), Required, [
                ("direction", Enum(&["off", "on"]), Required),
            ]),
            ("account", Ref("account"), Optional, []),
        ]
    }

    /// Every wire value `publish` puts on `shapes.list` for a known manifest:
    /// the names and their order, the dtype spellings, the required bools, the
    /// field lists, and the enum words — including the words of an enum nested
    /// inside a container, which is the case a flat read of the dtype would miss.
    #[test]
    fn a_well_formed_manifest_publishes_what_it_declares() {
        assert_eq!(
            WELL_FORMED.names(),
            vec!["status", "queued", "app", "moves", "account"]
        );

        let published = WELL_FORMED.publish();
        assert_eq!(published.len(), WELL_FORMED.len());

        let status = &published[0];
        assert_eq!(status.name, "status");
        assert_eq!(status.dtype, "struct:StateLevel");
        assert!(status.required);
        assert_eq!(status.fields.len(), 3);
        assert_eq!(status.fields[0].words, vec!["active", "idle"]);
        assert!(!status.fields[2].required, "`detail` is declared Optional");

        let app = &published[2];
        assert_eq!(app.dtype, "map<opaque>");
        assert!(
            app.fields.is_empty() && app.words.is_empty(),
            "an opaque map declares neither fields nor words"
        );

        assert_eq!(
            published[3].fields[0].words,
            vec!["off", "on"],
            "the words of an enum INSIDE a list reach the wire"
        );
        assert_eq!(published[4].dtype, "ref:account");
    }

    /// **AN EMPTY MANIFEST AND A FULL ONE ARE TOLD APART.**
    ///
    /// Both directions, because `is_empty` returning a constant `true` passes
    /// `Cells::NONE.is_empty()` on its own — and `NONE` is the value every shape
    /// without a shape-level cell writes, so a broken `is_empty` would read as
    /// *nothing is declared anywhere*.
    ///
    /// **THE FORCING.** `is_empty` returns `true`.
    ///
    /// ```text
    /// a manifest with five declared cells is not empty
    /// ```
    #[test]
    fn the_empty_manifest_is_the_only_empty_one() {
        assert!(Cells::NONE.is_empty(), "`NONE` declares nothing");
        assert_eq!(Cells::NONE.len(), 0);
        assert!(
            !WELL_FORMED.is_empty(),
            "a manifest with five declared cells is not empty"
        );
        assert_eq!(WELL_FORMED.len(), 5);
    }

    /// **TWO CELLS OF ONE NAME ARE REFUSED, AND ONE IS ACCEPTED, AND NONE IS
    /// NOT ONE.**
    ///
    /// **THE FORCING.** In `named_once`, `count_cells(..) == 1` becomes `>= 1`.
    ///
    /// ```text
    /// two cells of one name are refused at the declaration
    /// ```
    #[test]
    fn a_cell_name_is_declared_exactly_once() {
        let twice: &[Cell] = &[
            Cell {
                name: "status",
                dtype: Sc(Str),
                presence: Required,
                fields: &[],
            },
            Cell {
                name: "status",
                dtype: Sc(Int),
                presence: Required,
                fields: &[],
            },
        ];
        assert!(
            !named_once(twice, "status"),
            "two cells of one name are refused at the declaration"
        );
        assert!(
            named_once(WELL_FORMED.cells(), "status"),
            "…and one of a name is one"
        );
        assert!(
            !named_once(WELL_FORMED.cells(), "pending"),
            "a name nobody declared is not `once` either — the check cannot pass on absence"
        );
    }

    /// **`Ceiling` IS NOT A PRESENCE AN EMITTED THING CAN HAVE**, and the other
    /// two are.
    ///
    /// **THE FORCING.** `presence_ok` returns `true`.
    ///
    /// ```text
    /// `Ceiling` is refused on a cell: a cell is emitted, not asked for
    /// ```
    #[test]
    fn a_ceiling_is_refused_on_an_emitted_thing() {
        assert!(
            !presence_ok(Ceiling),
            "`Ceiling` is refused on a cell: a cell is emitted, not asked for"
        );
        assert!(
            presence_ok(Required) && presence_ok(Optional),
            "the two presences an emitted thing can have are both accepted"
        );
    }

    /// **A STRUCT LISTS ITS FIELDS; EVERYTHING ELSE LISTS NONE.**
    ///
    /// **THE FORCING.** In `fields_fit`, the `Struct` arm returns `true`.
    ///
    /// ```text
    /// a `Struct` cell with no fields is refused: list them, or declare it `Opaque`
    /// ```
    #[test]
    fn a_struct_owes_fields_and_a_scalar_owes_none() {
        let one = [Param::new("a", Sc(Int), Required)];

        assert!(
            !fields_fit(&Struct("X"), &[]),
            "a `Struct` cell with no fields is refused: list them, or declare it `Opaque`"
        );
        assert!(
            !fields_fit(&List(&Struct("X")), &[]),
            "…and the check reaches through a container to the struct at the leaf"
        );
        assert!(fields_fit(&Struct("X"), &one));
        assert!(fields_fit(&Map(&Struct("X")), &one));

        assert!(
            !fields_fit(&Sc(Int), &one),
            "a scalar with fields is a dtype and a field list describing two values"
        );
        assert!(
            fields_fit(&Opaque, &[]),
            "`Opaque` carries no fields by declaration, which is why it is the way out"
        );
    }

    /// **A FIELD NAME IS DECLARED EXACTLY ONCE, AND THE LOOKUP CANNOT PASS ON A
    /// CELL THAT IS NOT THERE.**
    ///
    /// The second half is the one that matters: the check names its cell as a
    /// literal, so a typo there would make it a check over an empty set.
    ///
    /// **THE FORCING.** In `field_named_once`, the `None` arm returns `true`.
    ///
    /// ```text
    /// a field check against a cell the manifest does not declare must not pass
    /// ```
    #[test]
    fn a_field_name_is_declared_exactly_once() {
        const TWICE: &[Cell] = &[Cell {
            name: "status",
            dtype: Struct("S"),
            presence: Required,
            fields: &[
                Param::new("state", Sc(Str), Required),
                Param::new("state", Sc(Str), Required),
            ],
        }];
        assert!(
            !field_named_once(TWICE, "status", "state"),
            "two fields of one name are refused"
        );
        assert!(
            field_named_once(WELL_FORMED.cells(), "status", "state"),
            "…and one of a name is one"
        );
        assert!(
            !field_named_once(WELL_FORMED.cells(), "pending", "state"),
            "a field check against a cell the manifest does not declare must not pass"
        );
        assert!(
            !field_named_once(WELL_FORMED.cells(), "status", "pending"),
            "a field nobody declared is not `once` either"
        );
    }

    /// **THE `Ref` NAMES A MANIFEST USES, FROM BOTH LEVELS, DEDUPLICATED, IN
    /// FIRST-SEEN ORDER.**
    ///
    /// Registration refuses a shape whose declared referents are not in this
    /// list, so an over-broad `ref_names` turns that refusal off silently. The
    /// four facts asserted separately: a cell-level ref is found, a FIELD-level
    /// ref is found, a ref at a container's leaf is found, and a name is listed
    /// once however many times it is carried. The last assertion is the negative
    /// control — a manifest that carries no ref names none.
    ///
    /// **THE FORCING.** In `ref_names`, drop the `fields` half of the chain.
    ///
    /// ```text
    /// a ref at a FIELD's leaf is one the manifest carries
    /// ```
    #[test]
    fn ref_names_are_every_referent_the_manifest_carries_once_each() {
        const REFERRING: &[Cell] = &[
            Cell {
                name: "account",
                dtype: Ref("account"),
                presence: Optional,
                fields: &[],
            },
            Cell {
                name: "pool",
                dtype: List(&Ref("pool")),
                presence: Required,
                fields: &[],
            },
            Cell {
                name: "origin",
                dtype: Struct("Origin"),
                presence: Required,
                fields: &[
                    Param::new("by", Ref("account"), Optional),
                    Param::new("at", Sc(Str), Required),
                ],
            },
        ];
        let carried = Cells::__declared(REFERRING).ref_names();
        assert_eq!(
            carried,
            vec!["account", "pool"],
            "two referents, in first-seen order, each listed once"
        );
        assert!(
            carried.contains(&"pool"),
            "a ref under a `list<…>` is one the manifest carries"
        );
        assert_eq!(
            Cells::__declared(&REFERRING[2..]).ref_names(),
            vec!["account"],
            "a ref at a FIELD's leaf is one the manifest carries"
        );
        assert!(
            Cells::__declared(&WELL_FORMED.cells()[..4])
                .ref_names()
                .is_empty(),
            "a manifest carrying no ref names no referent"
        );
    }

    /// **A DECLARED CELL IS FETCHABLE BY NAME, AND AN UNDECLARED ONE IS NOT.**
    /// `get` is how a producer's own test reads a declared vocabulary back out of
    /// the manifest to pin it against the const it was derived from.
    ///
    /// **THE FORCING.** In `get`, `find` becomes `self.cells.first()`.
    ///
    /// ```text
    /// assertion `left == right` failed: the cell fetched by name is that cell
    ///   left: "status"
    ///  right: "queued"
    /// ```
    #[test]
    fn a_cell_is_fetched_by_the_name_it_is_declared_under() {
        let queued = WELL_FORMED.get("queued").expect("`queued` is declared");
        assert_eq!(
            queued.name, "queued",
            "the cell fetched by name is that cell"
        );
        assert_eq!(queued.dtype.to_string(), "scalar:Int");
        assert!(
            WELL_FORMED.get("pending").is_none(),
            "`pending` is the name nobody declared; the manifest says so rather than stalling"
        );
    }

    /// The `const` proof's four values, read back. The fourth is `false` on
    /// purpose: cell `x` declares no field `y`.
    #[test]
    fn the_const_proof_holds() {
        assert_eq!(PROOF, (true, true, true, false));
    }
}
