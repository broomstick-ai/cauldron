//! # The producer's surface, as a CONSUMER's build input
//!
//! **The input twin of [`crate::cell`].** `cell` is how a PRODUCER declares
//! what its shapes carry, checked where the declaration is written. Nothing in
//! that arrangement ever reached the *consumer's* compiler. The declaration
//! travelled as `inventory.json` — committed, stamped with a `source_commit`,
//! vendored, hashed — and it was read by `tools/seamdiff`, a test tool, when
//! somebody chose to run it. That gap is paid for: the dead verbs
//! `thread.replace` and `thread.unpend` sat in a shipping face for months,
//! spelled as string literals, routed against nothing, answered `unknown
//! method` by the live daemon.
//!
//! This module is the two sealed tokens that turn the same data into a build
//! input. A consumer's seam kit vendors a GENERATED Rust module beside its
//! `inventory.json` — one `const` per method the producer dispatches, one per
//! cell it declares — and every act that depends on the surface takes a token
//! instead of a string:
//!
//! | act | took | takes |
//! |---|---|---|
//! | put a method name on the wire | `&str` | [`Dialed`], and only [`Method::at`] mints one |
//! | read a cell off a mirror | `&str` | [`Carried`], and only the generated module mints one |
//!
//! ## What that buys, precisely
//!
//! A name the producer does not serve has **no const to name**, so the refusal
//! is rustc's own — in the consumer's build, on the line that wanted it:
//!
//! ```text
//! error[E0425]: cannot find value `THREAD_UNPEND` in module `inventory::methods`
//! ```
//!
//! and the generated module is itself the list of what there is. That is the
//! form this chassis already settled on for a closed vocabulary: name the thing
//! that is missing and where the authority for it lives. The rejected
//! alternative on this arc was an arity-style check whose whole sentence was
//! *"expected an array with a size of 2"* — a build break that cannot say what
//! to add is a riddle.
//!
//! A name written as a LITERAL does not type-check at all: the parameter is a
//! token and `&str` is not one. There is no `request_raw` and no second door,
//! because an escape hatch IS the hole this module exists to close.
//!
//! ## The chassis never learns which producer it is checking
//!
//! Everything here is generic — a sealed token, an arity, one substitution. No
//! method name, no shape name, no cell name, no `cfg` for a producer. The
//! generated module lives in the **seam kit**, which is already the
//! producer-specific half of the vendored set and already carries "the same
//! thing" by hash, so two faces necessarily compile against one surface at one
//! commit. Generating per consumer would reintroduce exactly the drift the seam
//! exists to kill.
//!
//! ## Where the seal actually is
//!
//! Every token in here has private fields and no public constructor. Each has
//! one `#[doc(hidden)]` `const fn` that the generator emits calls to — the same
//! construction as [`crate::cell::Cells::__declared`] and the same reading: it
//! is the generator's door, not an author's. What makes such a door safe to own
//! is that the generator's INPUT is the producer's own dispatch table and cell
//! declarations; it cannot emit a const for a name the producer does not serve,
//! because it has never seen one.
//!
//! [`Dialed`] is where the seal does the most work, and it is worth saying why.
//! [`Dialable`] is an ordinary public trait: a consumer crate may write an
//! `impl` of it for a type of its own, and gain nothing by it. The only thing
//! such an `impl` can return is a `Dialed`, whose field is private, so the body
//! has no way to produce one. The trait is open and uninhabitable at the same time,
//! which is why the request door can bound on it for the error message without
//! the bound being a way in.
//!
//! [`Method::__served`] is not a blank cheque either. It const-asserts the one
//! thing a generator could get wrong and a hand-edit certainly would — that the
//! arity matches the template — so a token whose `at` would leave an unfilled
//! placeholder on the wire fails the build at the const item that declares it.
//!
//! ## What is NOT checked here, named so the gap is not read as coverage
//!
//! **TIME SKEW.** A consumer compiles against the inventory it vendored; the
//! producer may have moved since. No compile-time check in the consumer can see
//! that, by definition — the producer is not one of the consumer's build inputs,
//! and working inside that constraint is the whole design. It is caught
//! elsewhere: at the vendor bump (an offline re-scrape, refused if the surface
//! moved) and at link for contract versions. `tools/seamdiff --inventory` is
//! what asserts the generated module and `inventory.json` are the same surface
//! at the same `source_commit` — a test, and a test about the build's INPUT
//! rather than about the build.
//!
//! **THAT THE CARGO ARRIVES.** A declared cell reaching a known value is
//! liveness and honesty, not shape; the mirror's three states carry that half.
//! Declaration makes a consumer's *expectation* static. Nothing can make a
//! producer's *behaviour* static.

// ─────────────────────────────────────────────────────────────────────────────
//  A method the producer serves
// ─────────────────────────────────────────────────────────────────────────────

/// **A METHOD THE PRODUCER DISPATCHES, AS A TYPE.** `N` is how many placeholder
/// segments the template carries, so substitution has one shape per arity and an
/// unfilled placeholder cannot reach the wire.
///
/// There is no public constructor, no `From<&str>`, and the field is private: a
/// `Method` exists for exactly the templates the generated inventory module
/// declares, which is exactly what the producer's own source says it serves.
/// From outside this crate there is no struct literal to write —
///
/// ```compile_fail,E0451
/// let forged: shapewire::surface::Method<0> =
///     shapewire::surface::Method { template: "threads/unpend" };
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Method<const N: usize> {
    template: &'static str,
}

impl<const N: usize> Method<N> {
    /// **THE GENERATOR'S DOOR. Not an author's.**
    ///
    /// Hidden and double-underscored for the reason
    /// [`crate::cell::Cells::__declared`] is: the honesty of what goes in comes
    /// from the generator having read the producer's dispatch table, and a
    /// hand-written call is a claim nobody checked.
    ///
    /// It is not *unchecked*, though. The arity is const-asserted against the
    /// template, so a token that would put an unfilled placeholder on the wire —
    /// or swallow a segment the producer needs — breaks the build at the const
    /// item rather than answering `unknown_method` in production:
    ///
    /// ```compile_fail,E0080
    /// use shapewire::surface::Method;
    /// // The arity is one; the template has one placeholder segment. `Method<0>`
    /// // would dial `threads/{thread_id}/send` verbatim.
    /// const SEND: Method<0> = Method::__served("threads/{thread_id}/send");
    /// fn main() {
    ///     let _ = SEND.at();
    /// }
    /// ```
    #[doc(hidden)]
    pub const fn __served(template: &'static str) -> Self {
        // NO BRACES IN THE MESSAGE, deliberately: `assert!` reads its second
        // argument as a format string, so a placeholder spelled literally would
        // be parsed as a capture and fail to compile for the wrong reason. The
        // cell manifest's macro writes under the same constraint.
        assert!(
            holes(template) == N,
            "this method token's arity is not its template's. The N in `Method<N>` counts the \
             template's PLACEHOLDER SEGMENTS — a whole segment that is a name in braces — and \
             `at` fills exactly that many. Too small and a placeholder goes out unfilled, which \
             the producer answers `unknown_method`; too large and there is no segment left to \
             put the value in. WHAT IS MISSING: the arity that matches this template. \
             tools/make-inventory.py computes it from the producer's own method list, so a \
             disagreement here means this module was hand-edited instead of generated — \
             regenerate it.",
        );
        Self { template }
    }

    /// The template, in the producer's own spelling.
    ///
    /// **This is what a method LIST is made of.** A guard that keeps a second,
    /// text-scannable spelling of the method set reads these rather than a
    /// second set of literals, so the two spellings stay one fact. `const`,
    /// because that assertion is a `const` one.
    pub const fn template(&self) -> &'static str {
        self.template
    }
}

impl Method<0> {
    /// The path, for a method that names no subject.
    pub fn at(&self) -> Dialed {
        Dialed {
            path: String::from(self.template),
        }
    }
}

impl Method<1> {
    /// The path, with `subject` substituted into the template's one placeholder
    /// segment.
    ///
    /// **`subject` is not sanitised, and that is a decision rather than an
    /// omission.** A subject carrying a `/` becomes a longer subject span, not a
    /// different leaf: the producer's router (`crate::server::route`) reads
    /// everything between the family and the leaf as the subject, and refuses a
    /// request that names its subject twice. The producer's router is the
    /// authority on a malformed subject; an opinion about it here would be a
    /// second one.
    pub fn at(&self, subject: &str) -> Dialed {
        let mut path = String::with_capacity(self.template.len() + subject.len());
        // The subject is spent, not flagged: `take` is the one-placeholder rule
        // in the type. A template with a second brace segment — which the arity
        // assert makes impossible for `Method<1>` — would carry it through
        // literally rather than filling it twice.
        let mut subject = Some(subject);
        for (i, seg) in self.template.split('/').enumerate() {
            if i > 0 {
                path.push('/');
            }
            match subject.take_if(|_| is_hole(seg)) {
                Some(s) => path.push_str(s),
                None => path.push_str(seg),
            }
        }
        Dialed { path }
    }
}

/// **A CONCRETE PATH, READY FOR THE WIRE.** Sealed: [`Method::at`] is the only
/// thing that mints one, so the only strings a request door can carry are
/// substitutions of templates the producer serves.
///
/// The field is private, and that single fact is the seal. A forged path:
///
/// ```compile_fail,E0451
/// let forged = shapewire::surface::Dialed { path: String::from("threads/t-9/unpend") };
/// ```
///
/// and the same refusal through the trait, which is why implementing
/// [`Dialable`] downstream buys nothing:
///
/// ```compile_fail,E0451
/// use shapewire::surface::{Dialable, Dialed};
/// struct AVerbIInvented(String);
/// impl Dialable for AVerbIInvented {
///     fn dialed(self) -> Dialed {
///         Dialed { path: self.0 }
///     }
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialed {
    path: String,
}

impl Dialed {
    /// The substituted path, as it goes on the wire.
    pub fn path(&self) -> &str {
        &self.path
    }
}

/// **WHAT A REQUEST DOOR ACCEPTS — AND IT EXISTS FOR THE ERROR MESSAGE.**
///
/// A door typed on [`Dialed`] directly already refuses a string, but it refuses
/// with ``expected `Dialed`, found `&str` ``, which states a type equation and
/// not an obligation. The precedent on this arc is explicit: a compile error has
/// to name WHAT IS MISSING. A bound on this trait buys that sentence, and the
/// sentence is the only thing it buys.
///
/// Implemented for [`Dialed`] and nothing else. A downstream `impl` is not
/// forbidden by a sealing supertrait — it is simply inert, because a `Dialed`
/// cannot be constructed outside this crate (see [`Dialed`]'s own examples). The
/// escape hatch would be the hole, so there is none to grant.
///
/// ```compile_fail,E0277
/// use shapewire::surface::Dialable;
/// fn door<M: Dialable>(_method: M) {}
/// fn main() {
///     door("threads/t-9/unpend");
/// }
/// ```
#[diagnostic::on_unimplemented(
    message = "a method name cannot be written here; it is minted from the producer's surface",
    label = "not a method token",
    note = "The only strings this door can put on the wire are substitutions of templates the \
            PRODUCER declares, so it takes a `shapewire::surface::Dialed` — which only \
            `Method::at` mints, from a `Method` const the generated seam inventory emits. A \
            string literal is a name nobody checked, which is how `thread.replace` and \
            `thread.unpend` shipped: spelled in a face, routed against nothing, answered \
            `unknown method` by the daemon.",
    note = "WHAT IS MISSING: the method's const. Write `inventory::methods::<NAME>.at(subject)` \
            (or `.at()` for a method that names no subject). If the method is not in that \
            module the producer does not serve it, and the only way to send it is for the \
            producer to gain it and the inventory to be regenerated — there is no raw door."
)]
pub trait Dialable {
    fn dialed(self) -> Dialed;
}

impl Dialable for Dialed {
    fn dialed(self) -> Dialed {
        self
    }
}

/// **HOW MANY PLACEHOLDER SEGMENTS A TEMPLATE HAS.** `const`, because
/// [`Method::__served`] asserts on it.
pub const fn holes(template: &str) -> usize {
    let b = template.as_bytes();
    let mut found = 0;
    let mut seg = 0;
    let mut i = 0;
    while i <= b.len() {
        if i == b.len() || b[i] == b'/' {
            if is_hole_at(b, seg, i) {
                found += 1;
            }
            seg = i + 1;
        }
        i += 1;
    }
    found
}

/// **IS `item` IN `list`?** `const`, so a consumer can assert that two lists it
/// keeps for two different readers have not drifted apart — a set of method
/// tokens and the flat template list a text-scanning guard walks, for instance.
/// Exact equality: this answers membership, not routing.
pub const fn lists(list: &[&str], item: &str) -> bool {
    let mut i = 0;
    while i < list.len() {
        if same_bytes(list[i].as_bytes(), item.as_bytes()) {
            return true;
        }
        i += 1;
    }
    false
}

const fn same_bytes(a: &[u8], b: &[u8]) -> bool {
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

/// A WHOLE SEGMENT in braces, by byte range — `b[start..end)`.
///
/// One definition of "a placeholder", read by both the counter and the filler,
/// so the two cannot disagree about what `at` will substitute. Three bytes at
/// least: an empty pair of braces names nothing, so it is not a placeholder and
/// there would be nothing to fill it with.
const fn is_hole_at(b: &[u8], start: usize, end: usize) -> bool {
    end >= start + 3 && b[start] == b'{' && b[end - 1] == b'}'
}

const fn is_hole(seg: &str) -> bool {
    is_hole_at(seg.as_bytes(), 0, seg.len())
}

// ─────────────────────────────────────────────────────────────────────────────
//  A cell the producer declares
// ─────────────────────────────────────────────────────────────────────────────

/// **A CELL THE PRODUCER DECLARES, PAIRED WITH THE SHAPE THAT DECLARES IT, AS A
/// TYPE.**
///
/// The pair travels together because a cell name alone is not a fact: `limits`
/// is declared on two shapes, and `threads` is a cell on one shape and the name
/// of another. A read vocabulary that took the two apart could name a cell of
/// the wrong shape and still route clean.
///
/// There is no public constructor and the fields are private. A `Carried` exists
/// for exactly the cells the generated inventory module declares, which is
/// exactly the per-key and shape-level blocks as the producer wrote them.
///
/// ```compile_fail,E0451
/// let forged = shapewire::surface::Carried {
///     shape: "threads",
///     name: "pending",
///     shared: false,
/// };
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Carried {
    shape: &'static str,
    name: &'static str,
    shared: bool,
}

impl Carried {
    /// **THE GENERATOR'S DOOR.** See [`Method::__served`] for the reading; there
    /// is nothing here a const-assert could check that the generator's input has
    /// not already decided.
    #[doc(hidden)]
    pub const fn __declared(shape: &'static str, name: &'static str, shared: bool) -> Self {
        Self {
            shape,
            name,
            shared,
        }
    }

    /// The shape this cell is declared on.
    pub const fn shape(&self) -> &'static str {
        self.shape
    }

    /// The cell's name, as a row spells it.
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// `true` for a SHAPE-LEVEL cell, `false` for a per-key one.
    ///
    /// Carried on the token rather than chosen by the reader, because which
    /// block a cell lives in is the producer's statement — and a reader that
    /// picked the wrong block would read "not hydrated yet" for the life of
    /// every process, which is a silence and not an error.
    pub const fn is_shared(&self) -> bool {
        self.shared
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **THE ARITY CHECK RUNS IN `const` CONTEXT, AND THAT IS THE WHOLE CLAIM.**
    /// If [`holes`] were an ordinary function this item would not compile, so its
    /// presence is the evidence that [`Method::__served`]'s assertion is a BUILD
    /// failure rather than a runtime one. The `compile_fail` doctest on
    /// `__served` is the other half: this proves the check is const-evaluated,
    /// that one proves it refuses from outside the crate.
    const ARITIES_AT_COMPILE_TIME: (usize, usize, usize) = (
        holes("threads/create"),
        holes("threads/{thread_id}/send"),
        holes("accounts/login/{session_id}/cancel"),
    );

    const CREATE: Method<0> = Method::__served("threads/create");
    const SEND: Method<1> = Method::__served("threads/{thread_id}/send");
    const CANCEL_LOGIN: Method<1> = Method::__served("accounts/login/{session_id}/cancel");

    #[test]
    fn the_arities_were_computed_before_the_test_ran() {
        assert_eq!(ARITIES_AT_COMPILE_TIME, (0, 1, 1));
    }

    #[test]
    fn a_subjectless_method_dials_its_own_template() {
        assert_eq!(CREATE.at().path(), "threads/create");
        assert_eq!(CREATE.template(), "threads/create");
    }

    #[test]
    fn a_subject_lands_in_the_placeholder_and_nowhere_else() {
        assert_eq!(SEND.at("t-9").path(), "threads/t-9/send");
        assert_eq!(
            CANCEL_LOGIN.at("s-1").path(),
            "accounts/login/s-1/cancel",
            "the placeholder is the THIRD segment, not the second"
        );
    }

    /// The documented non-promise, pinned so a later "fix" has to argue with a
    /// test: a subject with a `/` widens the subject span and does not move the
    /// leaf. The producer's router is what refuses it.
    #[test]
    fn a_subject_is_substituted_and_not_sanitised() {
        assert_eq!(SEND.at("a/b").path(), "threads/a/b/send");
    }

    /// **A PLACEHOLDER IS A WHOLE SEGMENT, NOT A SUBSTRING** — in the counter
    /// and in the filler, which read one predicate. So `holes` cannot be talked
    /// up by prose inside a path, and `at` cannot be made to substitute into it.
    #[test]
    fn only_a_whole_segment_is_a_placeholder() {
        assert_eq!(holes("a/b{c}d/e"), 0);
        assert_eq!(
            holes("a/{}/e"),
            0,
            "an empty pair of braces names nothing, so there is nothing to fill"
        );
        assert_eq!(holes("a/{b}/{c}"), 2);
        assert_eq!(holes("{a}"), 1, "a one-segment template can be a placeholder");

        const BRACED_PROSE: Method<1> = Method::__served("a/{b}/c{d}e");
        assert_eq!(
            BRACED_PROSE.at("X").path(),
            "a/X/c{d}e",
            "the second segment is the only placeholder; the brace inside the third is text"
        );
    }

    /// **MEMBERSHIP IS EXACT, AND IT CANNOT PASS ON ABSENCE.** The one way a
    /// drift assertion built on this could be a decoration is by answering
    /// `true` for a list that does not contain the item, so that is what this
    /// forces — in both directions, over the same list.
    #[test]
    fn membership_is_exact() {
        const L: &[&str] = &["threads/create", "threads/{thread_id}/send"];
        assert!(lists(L, "threads/create"));
        assert!(lists(L, "threads/{thread_id}/send"));
        assert!(
            !lists(L, "threads/{id}/send"),
            "a different spelling of the same route is a different string"
        );
        assert!(!lists(L, "threads/create/"), "and so is a longer one");
        assert!(
            !lists(&[], "threads/create"),
            "an empty list contains nothing"
        );
    }

    /// The token carries both halves of the fact, and the trait that fronts the
    /// request door passes a `Dialed` through unchanged.
    #[test]
    fn a_cell_token_carries_the_shape_and_the_level() {
        const STATUS: Carried = Carried::__declared("threads", "status", false);
        const FLEET_SIZE: Carried = Carried::__declared("threads", "fleet_size", true);
        assert_eq!(
            (STATUS.shape(), STATUS.name(), STATUS.is_shared()),
            ("threads", "status", false)
        );
        assert!(FLEET_SIZE.is_shared());
    }

    #[test]
    fn the_request_door_passes_a_dialed_through() {
        fn door<M: Dialable>(method: M) -> String {
            method.dialed().path().to_string()
        }
        assert_eq!(door(SEND.at("t-1")), "threads/t-1/send");
    }
}
