//! **`cauldron soup import | regions | region | nodes | search | get | dump |
//! dumps | dump-get | dump-remove`** — a cauldron's soup, offline, for an agent that is not
//! the soup daemon, and the dumps kept beside it.
//!
//! - `import` lands records written in the PUBLISHED shape
//!   (`schema/soup/workspace-v6/`) as ONE write, all or nothing, after the
//!   spine's own door has judged every one (`spine::migrate::records`).
//! - `regions`, `region`, `nodes`, `search` and `get` read the soup's pool
//!   through a connection that cannot write (and `nodes` the spine's commit
//!   order the same way), so an agent can see where records belong and what
//!   the soup already says. Every read is JSON, bounded by a `--limit` with
//!   a default, and takes `--max-chars`; what it cut it lists.
//!
//! # `import --dry-run` while a soup daemon runs
//!
//! This release runs no soup daemon, but a real import still refuses when
//! the environment's seam table shows a live one, because it opens the
//! store directly and a daemon may hold it. A dry
//! run is allowed, because it never touches the live store with anything
//! but the read-only connection `get` already uses: each `*.db` file is
//! copied with `VACUUM INTO` from that connection into a private temporary
//! directory (`spine::offline::snapshot_copy`), and the spine's writing open
//! and the door's validate mode run on the COPY. The answer is as of that
//! snapshot.
//!
//! # How `import` opens the store, and why that is safe
//!
//! The spine's only production open is `spine::Cauldron::open`, which runs
//! the recovery pass and the tranche pass before it returns, and those
//! WRITE (roll forward, abort, sweep orphan locks, reset tranche deadlines).
//! That is why the other verbs never open a soup that way
//! (`cauldrons.rs`). `import` has to write — the one write door is reached
//! only through a handle from that open — so it opens only a soup that is
//! (1) held by no running soup daemon and (2) AT REST, so those passes have
//! nothing to do and the import's commit is the only write:
//!
//! 1. the environment's seam table is read; a `soup` row whose process is
//!    alive refuses, and a table that cannot be read refuses (no proof);
//!    then the soup's writer lock (`<soup>/writer.lock`) is taken exclusive
//!    and held to the end, so no second import or `recover` writes beside
//!    it;
//! 2. `spine::offline::rest_check` reads `spine.db` read-only: not halted,
//!    the current spine schema, no un-cleared transaction row, no lock row,
//!    no live tranche, no quiesce, every pool with its seal column. Any
//!    reason refuses, naming it; an unreadable spine refuses;
//! 3. only then `Cauldron::open`, and its own report is checked: a pass
//!    that did anything after all (a writer that slipped in between) is
//!    reported and the import refuses before it submits.
//!
//! The import is then dry-run through the door's VALIDATE mode (no row, no
//! lock) and, unless `--dry-run`, landed as one statement under one spine
//! row. The spine's locks are tables in `spine.db`, arbitrated across
//! processes, so a daemon that starts in the middle meets an ordinary
//! concurrent writer: the worst case is a restart of the import's
//! attempt, never a half-landed batch.
//!
//! # An interrupted import, and `recover`
//!
//! Killing an import part way never lands part of it, but it can leave the
//! spine NOT AT REST: its transaction row un-cleared, its locks held. Every
//! later import then refuses at step 2, and with no soup daemon nothing
//! else would ever settle it. `cauldron soup recover` does: under the same
//! two proofs (no live soup daemon, the writer lock held exclusive) it runs
//! `Cauldron::open`, whose recovery pass aborts a row that was never
//! decided (nothing of that import lands) and rolls a decided row forward
//! (all of it lands, once), and reports which of the two happened, with
//! the ids that landed. The difference decides what the user does next:
//! a rolled-back import may be run again, a landed one must not be (with
//! `--remint` it would land every record a second time).
//!
//! # A batch that already landed
//!
//! Every import writes its batch's content digest (SHA-256 of the pool name
//! and the records as read, before any `--remint`) into its own transaction
//! row's provenance, the `dispatch` text of its `agent` initiator. Before it
//! writes, an import looks for a DECIDED row carrying the same digest and,
//! finding one, refuses as `batch-already-landed` unless `--allow-duplicate`
//! is passed. That covers an import `recover` finished, too, since a decided
//! row is one that landed. No table or column is added, so a store from an
//! earlier release opens unchanged; its earlier imports carry no digest and
//! are not recognised.
//!
//! # While the `mcp` service holds the soup
//!
//! A cauldron granted to the MCP door with `write` is held open by the
//! environment's `mcp` service, which keeps the soup's writer lock and
//! writes `mcp <pid>` into it. A real import that finds the lock held so,
//! and the service's seam row live with that pid, sends its records to the
//! service as one `soup_put` and prints the answer in the import's own
//! shape (plus `forwarded_to`); a refused batch and a batch that already
//! landed print exactly as a direct import's do. `recover` is refused,
//! naming the service, which recovers the soup itself when it opens it. An
//! ungranted cauldron is never held, and the import runs directly as before.
//!
//! # Dumps
//!
//! `dump` keeps a file's text (or stdin's) as a dump in the cauldron's dumps
//! store beside its soup (`soup_door::dumps`): `DM-<n>` in state `received`,
//! one transaction, for later processing. Like `import`, it writes directly
//! when nothing else holds the store, and when the `mcp` service holds it
//! (the cauldron is granted `dump`, and a dump came through the door) it is
//! sent to the service as one `soup_dump`. `dumps` and `dump-get` read the
//! store through connections that cannot write.
//!
//! The logic lives in the `soup-door` library (`libs/soup-door`), which
//! answers every verb as JSON or a typed error; this module parses the
//! arguments and prints.
//!
//! Like `ctl`, every verb needs the environment NAMED (`--env`, a leading
//! `--env`, or `$CAULDRON_ENV`): it never defaults to prod. The cauldron is
//! always named too.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use soup_door::import::{ImportOptions, ImportOutcome};
use soup_door::reads::ReadOpts;
use soup_door::Target;

const USAGE: &str = "\
cauldron soup — a cauldron's soup, offline

    cauldron soup import <file-or-dir> [--cauldron <name>] [--env <env>] [--dry-run] [--remint]
                         [--allow-duplicate] [--agent <label>]
        Land records written in the published shape (schema/soup/workspace-v6/README.md;
        a record of type `cluster` in schema/soup/cauldron-v1/README.md's, which lands in the
        soup's cluster pool in the same write, so one batch may hold a region, a cluster and
        its `claims` of that region): one file holding a list (or one record), or a directory
        of one-record files; JSON, or YAML for .yaml/.yml. Every record is judged by the soup's own door; the JSON
        printed gives each one's disposition (accepted, or refused with the grammar's
        reason). A `meta.cites` entry whose kind is not in the README's table, or that lacks
        its kind's keys, refuses its record as `bad-cite`. Without --dry-run the whole set
        lands as ONE write when every record is accepted, and nothing lands otherwise. Ids
        are carried as written and a collision refuses the import; --remint treats them as batch-local, gives each record the
        next free id for its type, rewrites references and [[links]] inside the batch, and
        prints the old -> new map (`id_map`) when the batch lands or a dry run validates; a
        refused batch prints none, since its ids were never taken. Under --remint a label
        may not equal an id the soup already holds (refused, `label-shadows-existing`), so
        a bare id that is not a label
        always names the existing node; `<ID>@<pool>` names it explicitly. An edge to an id
        outside the batch must name a node the soup already holds. A real import is refused
        while another import or recover is writing to the same soup, while the soup is not
        at rest (an earlier import was interrupted: see `recover`), and when the
        environment's seam table cannot be read or lists a running `soup` service (this
        release ships no soup service, so in practice that means only an unreadable seam
        table); --dry-run is then judged against a read-only snapshot. A batch whose records
        (as read, before any --remint) are identical to one that already landed in this
        pool is refused as `batch-already-landed`, with when it landed and how many records
        it held: under --remint every record would land again under new ids, and without it
        the carried ids would collide and the import would be refused; --allow-duplicate
        skips this check. Each import records the batch's digest in its own spine row, so
        batches landed by an earlier release are not recognised. --agent records who wrote
        (default `outside-agent`). Every refused batch prints one shape: `outcome`
        `refused`, a one-word `refusal` (`records-refused`, `landing-refused` or
        `batch-already-landed`), `accepted`, `refused` and `records` (empty when the batch
        was refused before any record was judged).
        While the cauldron is granted to the MCP door and the `mcp` service holds its soup
        open (`cauldron mcp status`), a real import is sent to that service and lands
        through it: the same judgement, the same JSON (with `forwarded_to`), the same
        batch-already-landed check and --allow-duplicate; the service records the writer
        as `mcp:cli`, not --agent, and writes only the soup's own pools (`ws`, the cluster
        pool for cluster records, and a user grammar's pool for its types). A dry run is never sent.
        A batch over the 16 MiB the service reads in one request is refused before it is
        sent (never split): import it in smaller files, or revoke the grant (or stop the
        service) and import directly.
        A batch with a record or edge in a region a ripple holds (`soup regions` marks it
        `locked`; a header change or re-parenting of a region above one, `locked_below`, too)
        is refused whole as `region_locked`, at once, naming the holder and until when, with
        nothing written. Try again after that time, or, if the records belong in another
        region, write them there.
    cauldron soup import-corpus <file-or-dir> [--cauldron <name>] [--env <env>] [--dry-run]
                         [--allow-duplicate] [--agent <label>]
        Land the older corpus format (the terminal interface's seed corpus, schema version 5):
        a seed file (`clusters`, `corpus`), a bare corpus (`regions`, `nodes`), one record, or a
        directory of one-record files (searched recursively). Each region and node becomes a
        workspace-v6 record (`narrows` and `in_region` become edges, `cite: <thread>#<msg>`
        becomes `operator`, the old id is kept in `meta.imported_from`), and each cluster (and
        each child cluster) a cauldron@1 `cluster` record, its name the handle, its description
        the boundary, with a `claims` edge to each region of the input it names. The batch goes
        through `soup import --remint`: the same judgement, all or nothing, the same refusals
        and forwarding, new ids for every record. The JSON adds `corpus`: what was carried,
        `id_map` by old id (when the ids hold), and `not_carried`, every part of the input the
        schemas cannot hold, each with its reason: a cluster's tasks and other parts, its claim
        of a region not in the input, a child's nesting, a cluster whose name or description is
        outside the grammar's bounds, a node type outside the ten, a reference to an id not in
        the input, the seed's messages and dashboards, and every unknown key (carried verbatim
        in meta.imported_from.unmapped).
    cauldron soup recover [--cauldron <name>] [--env <env>] [--abandon-merge [--keep-partial-merge]]
                          [--abandon-undo [--lose-undo] [--set-aside-later-writes]] [--settle-merge <MG-n>]
        Settle a soup that an interrupted import left not at rest: an import that never
        reached its commit decision is rolled back (none of it lands), one that did is
        finished (all of it lands, once). Refused while another import or recover is
        writing to the soup, and on the same seam-table check as import; a soup already at
        rest is not touched. Prints (JSON on stdout, the same in words on stderr) which of
        the two happened: `interrupted_import` is `landed` (with how many records and their
        ids: do NOT import that batch again; under --remint every record would land twice),
        `rolled-back`
        (none of it landed: importing it again is safe) or `none` (nothing to recover).
        Also says what still stands in the way if anything does. Refused while the `mcp`
        service holds the soup: it recovers the soup itself when it opens it, and
        `cauldron mcp status` shows what that recovered. It also mends a region-lock table
        (`<soup>/region-locks.json`) that cannot be read, which otherwise refuses every
        write, rename and remove: holding the soup's writer lock, and only when no ripple
        run of the cauldron is live in the ripples journal (a run with no `done` line whose
        process is running and whose last line is under an hour old), it moves the file
        aside to `region-locks.json.damaged-<UTC time>` and the soup holds no lock; with a
        run live it refuses, naming it, and changes nothing. When it sets one aside, its
        output carries `region_locks` (`set_aside`, with `moved_to`). A `cauldron merge`
        into the cauldron that was cut short is resolved here too (`merge` in the output):
        abandoned when it was cut short before it held the cauldron (nothing of it
        landed), finished when it held it (all of it lands, once), or, when it cannot be
        finished, the cauldron put back exactly as the merge's snapshot holds it; the
        blobs a merge cut short before its journal staged are swept (`merges_swept`). An
        undo of a merge (`cauldron merge --undo`) that was cut short is finished first
        (`merge_undo` in the output; `undo-finished` when nothing else was to do). With
        --abandon-merge, a merge cut short is not finished: its hold is lifted and what
        of it landed stays (listed in `merge.landed`); for a merge recover cannot finish
        nor put back (its snapshot or staged blobs gone), so the cauldron is never
        left refusing every writer; it also sets aside a merge journal that cannot be
        read and lifts a merge's hold that nothing names; a merge recover can still
        settle is refused unless --keep-partial-merge. With --abandon-undo, an undo of
        a merge that cannot be finished (its snapshot moved, deleted or changed, or its
        marker unreadable) is given up and the merge kept: the cauldron ends exactly as
        the merge left it (soup, dumps and record), or it refuses and changes nothing;
        a later write to a store the undo had put back needs --set-aside-later-writes,
        and an undo that can still be finished needs --lose-undo. Both abandons are
        the owner act `merge` when the merge or undo was the owner's. `merge_undo`
        says what was done (`end_state`). --settle-merge <MG-n> completes a cauldron that
        `cauldron merge --status` calls `torn`: a merge record left over when neither store
        holds the merge is moved aside; where part of the merge was taken away, it settles
        the merge as it stands (its record then names what was lost) once no record that
        can be changed cites a missing dump, or refuses, listing those records and how to
        re-point their cites (`soup cite`; merging the source again does not repair them).
    cauldron soup regions [--cauldron <name>] [--env <env>] [--max-chars N]
        Every region: id, handle, boundary, flags, the regions it narrows or relates to,
        and how many nodes sit in it. A region a ripple holds is marked `locked` (holder,
        purpose, since, until); a region above a held one, `locked_below`.
    cauldron soup region <id> [--cauldron <name>] [--env <env>] [--depth N] [--limit N] [--max-chars N]
                         [--include-superseded] [--include-rejected]
        One region: its record, the regions it narrows (parents) and that narrow it
        (children) to --depth levels (default 1), the regions it relates to either way, and
        its direct members (id, type, state, handle), newest first, at most --limit (50), and
        the clusters that claim it (`clusters`: id, handle, note).
    cauldron soup nodes [--cauldron <name>] [--env <env>] [--region <id>] [--type <t>]
                        [--state <s>] [--limit N] [--max-chars N] [--include-superseded]
                        [--include-rejected] [--review pending|reviewed]
        Nodes as {id, type, state, handle, regions}, newest first (by the spine's commit
        order; one write's nodes by id prefix, then serial, highest first), at most --limit
        (default 50); --region keeps a region's direct members. --type cluster lists the
        clusters instead, each with the regions it claims (--region: those claiming it).
    cauldron soup search <query> [--cauldron <name>] [--env <env>] [--region <id>] [--type <t>]
                         [--limit N] [--max-chars N] [--include-superseded]
                         [--include-rejected] [--review pending|reviewed]
        Lexical search over handles and bodies, and over cluster handles and boundaries
        (--type cluster: clusters only): every word of the query a term, found
        case-insensitively as a substring; ranked by how many terms match, then handle hits over body hits, then newest.
        Prints {id, type, state, handle, regions, score, snippet}, at most --limit (20);
        the snippet is --max-chars long (default 160) around the first hit.
    cauldron soup get <id>... [--cauldron <name>] [--env <env>] [--max-chars N] [--edges-in]
        Those nodes as records in the published shape, with their outgoing edges. A node
        another node supersedes (an inbound `supersedes` edge) is answered all the same, with
        `superseded_by: [ids]`, which is read from the edges and is not a field: leave it out
        when writing the record again. With --edges-in, each record also lists `edges_in`:
        the edges that arrive at it from any pool (`from`, `kind`, `from_pool` when another),
        at most 500 (`edges_in_total` when there are more); an edge names one version of a
        record, so after a supersede the edges into the old one stay on it. A cluster (CL-<n>) is answered with `claims` (each
        claimed region, its handle and the claim's note), read from its edges, not a field.
        An edge's note longer than 1,000 characters (written before notes were bounded) is
        shown cut to 1,000, named in `truncated`.
    cauldron soup claim <cluster> <region> [--cauldron <name>] [--env <env>] [--release]
                        [--note <text>] [--agent <label>]
        A cluster (CL-<n>: an area of the surface, a name and a boundary description, written
        as a `cluster` record) claims a region (R-<n>), or with --release gives the claim up.
        One write; idempotent: a claim already there prints `already-claimed`, a release of
        one that is not prints `not-claimed`, and neither writes. --note is kept on the claim
        (at most 1,000 characters, as every edge's note: a longer one is refused).
        A claim is not a lease: it holds nothing and assigns no work. Refused for a cluster or
        region the soup does not hold, an id that is not a region, and while a ripple holds
        the region (`region_locked`, as for an import). Written directly, or, while the `mcp`
        service holds the soup, sent to it as `soup_claim`.
    cauldron soup link <from> <kind> <to> [--cauldron <name>] [--env <env>] [--note <text>]
                       [--agent <label>]
        Add one edge between two records the soup holds (`soup_link` from a shell), judged by
        the grammar first: `cauldron soup link DEC-2 ref T-4 --note evidence`. Not
        `cauldron link`, which writes a project's `.cauldron` marker. Written directly, or,
        while the `mcp` service holds the soup, sent to it as `soup_link`.
    cauldron soup unlink <from> <kind> <to> [--cauldron <name>] [--env <env>] [--agent <label>]
        Remove one edge between two records the soup holds (`soup_unlink`): the record keeps
        no wrong edge (`get` stops showing it) and the change feed records an `unlink`.
        Idempotent: an edge that is not there prints `not-linked` and writes nothing. Refused
        by name: a record's last `in_region` (or the last edges its type requires: add the
        replacement first), a `supersedes` edge (history: write a new record instead), a
        cluster's `claims` (use `soup claim --release`), and while a ripple holds a region it
        touches (`region_locked`). Written directly, or sent to the `mcp` service.
    cauldron soup cite <id> --add <json> | --remove <index-or-json> [--cauldron <name>]
                       [--env <env>] [--agent <label>]
        Add or remove ONE entry of a record's `meta.cites` (`soup_cite`): --add takes one
        citation as JSON (`{\"kind\":\"url\",\"url\":\"…\"}`), judged by the import's citation table;
        --remove takes its index (from 0, as `get` lists them) or a JSON object its entry
        matches. Only `meta.cites` is written. Idempotent: `already-cited`, `not-cited`.
        Written directly, or sent to the `mcp` service.
    cauldron soup changes [--cauldron <name>] [--env <env>] [--since <cursor>] [--wait <secs>]
                          [--limit N] [--max-chars N]
        What landed since a cursor, oldest first, as `soup_changes` answers it: `changes`
        (each write: `op` create, link, unlink or update, with the ids it touched; cluster
        and claim writes included, on the same cursor), `dumps`, and `cursor` to pass as
        the next --since. Without --since, only the current cursor. --wait N (at most 60,
        with --since) waits up to N seconds for something to land after the cursor and
        prints as soon as it does, or when the wait ends with nothing (`waited.ended` is
        `landed` or `timeout`); it looks at the store twice a second meanwhile and holds
        no lock. --max-chars bounds the whole answer. A cursor of another cauldron is
        refused (`cursor_unknown`), one ahead of the soup too (`cursor_stale`). The set of
        `op` values can grow in a later release: a program reading this must ignore an op
        it does not know.
    cauldron soup dump <file|-> [--cauldron <name>] [--env <env>] [--title <t>] [--source <s>]
                       [--wait-secs N]
        Keep the file's text (or stdin's, with -) as a dump in the cauldron's dumps store,
        beside its soup (not as a soup node): DM-<n>, in state `received`, as one write,
        for later processing: by hand (`cauldron ripples run <id>`), or automatically when
        the owner turned that on for the cauldron (`cauldron ripples on`); the receipt's
        `automatic` says which. --wait-secs N (at most 120) waits that long at most for
        automatic processing and prints what it made (`processing`: the state, the run, the
        record ids). At most 1 MiB
        of UTF-8 text; a larger one is refused as `too_large` (split it). Prints a receipt
        (id, bytes, sha256), never the text. While the `mcp` service holds the cauldron's
        dumps store (`cauldron mcp status`), the dump is sent to it and lands through it,
        printed the same way plus `forwarded_to`.
    cauldron soup dumps [--cauldron <name>] [--env <env>] [--state <s>] [--kind text|session] [--limit N]
        The cauldron's dumps, newest first: id, state, kind, title, bytes, sha256, source,
        when received. Never their text. At most --limit (50).
    cauldron soup dump-get <id> [--cauldron <name>] [--env <env>] [--offset N] [--max-chars N] [--raw]
                               [--view raw|text] [--turn-offset N]
        One dump's content from byte --offset (default 0): JSON whose whole text is at most
        --max-chars (default 12000, at most 50000, at least 1000: smaller is refused), with
        `next_offset` while there is more.
        --raw prints the whole content, verbatim, and nothing else (--offset and --max-chars
        do not apply): a text dump's text, or a session dump's transcript exactly as stored.
        To a terminal (not a file or a pipe), its control and invisible characters (escape
        sequences, bidirectional overrides, zero-width characters) are shown escaped, as
        \\u{1B}, and its lines are kept.
        --view text (a session dump only) answers only what the person typed and what the
        assistant wrote as text, as turns with `at` (the byte its line starts at), `role` and
        `text`. Everything else is left out and counted in `elided` by kind: tool calls and
        results, thinking, `!` shell commands and their output, local commands' output,
        slash-command wrappers, system reminders, other text Claude Code writes itself,
        isMeta lines, attachments, images and other lines. The raw view (and the stored
        blob) still holds the whole transcript. Pages end between turns and read the
        same each time; read on with --offset <next_offset>
        and, after a turn longer than a page (cut, with `chars` its length),
        --turn-offset <next_turn_offset>.
    cauldron soup dump-remove <id> --env <env> --cauldron <name> [--yes]
        Remove one dump's content for good (a dump holding a secret, say). Without --yes it
        is a dry run: it removes nothing, prints what it would do and exits 2. With --yes,
        in two phases. First the dump becomes a tombstone in state `removing` (its id,
        kind, size and when it was received and removed are kept; its title, source,
        cites, refs and sha256 are dropped), and from then on no read answers its
        content. Then its blob is deleted unless another dump holds the same bytes (named
        in `same_bytes_in`; remove those too), its text, title, source note and sha256 are
        erased from the dumps store's record of its writes, the store's databases are
        compacted, so nothing of it is left in the cauldron's directory, and only then is
        its state `removed`. Reading it afterwards answers the tombstone; a soup record
        that cites it still finds it, as removed. A removal cut short (a kill, a crash)
        leaves the dump `removing`, and reading it says the erasure is unfinished and
        prints the command that finishes it: running this again finishes it, and so does
        the next open of the dumps store for writing (a `soup dump`, or the `mcp`
        service's). Copies made elsewhere (backups, a moved-aside cauldron) are not
        touched. --env and --cauldron must be given as flags (not a variable or a marker).
        Refused while the `mcp` service holds the cauldron's dumps store: revoke its
        grant (`cauldron mcp revoke <name> --env <env>`) or stop the service, run this,
        then grant again. Never an MCP tool.
    cauldron soup grammar check | add | list | show ...
        A cauldron's grammars (ontologies): judge a grammar file, register a user-defined
        one as a new pool (an owner act), list the pools, print one's grammar. Records of
        a user grammar's types then go to its pool by type through `soup import` and
        `soup_put`. `cauldron soup grammar help`.
    cauldron soup approve <id>... --env <env> --cauldron <name> [--unapprove]
        Approve records a ripple wrote (`cauldron ripples run`): each named record that
        reads as `unreviewed` gets `meta.approved: {at, by}` and reads as
        reviewed from then on; with --unapprove, each approved one reads as unreviewed
        again. All in one write, through the running `mcp` service, audited in its
        calls.jsonl. Approving is the owner's act: this runs with its input and output on
        an interactive terminal (`by: \"tty\"`), or without one only where the owner opened
        `approve` to agents (`cauldron owner-acts help`; `by: \"agent\"`); a ripple never
        can. An id that is not a ripple's record, is rejected, or is already in
        the asked state, is listed in `skipped` with why. `cauldron ripples review` lists
        the records waiting, and `cauldron ripples approve | reject` decides them by dump,
        by run or by id. Like those, it needs the running `mcp` service to be this
        release's or later, and refuses (writing nothing) an older one.
    cauldron soup help

  Every verb prints JSON on stdout and exits non-zero on a refusal. Reads open the store
  so that they cannot write. --max-chars cuts any longer text field, and the output's
  `truncated` lists what was cut; a list cut by --limit says how many `more` there are.
  A listed region or viewer has no `state` key: those types carry no state.
  `nodes`, `search` and `region`'s members leave out a superseded node (one another node
  supersedes) and say how many they left out in `superseded_hidden`; --include-superseded
  lists them too, each with its `superseded_by`. `total` and `members_total` count what
  is listed, before --limit.
  A record a ripple wrote (`cauldron ripples run`) is unreviewed until its owner approves
  or rejects it (`cauldron ripples review | approve | reject`, or `cauldron soup approve`):
  every read shows it, marked `unreviewed: true`.
  --review pending lists only those; --review reviewed only the rest; --review by-tty
  or by-agent only the records approved or rejected by a person at a terminal, or by an
  agent the owner allowed (each listed with `decided_by`).
  A record its owner rejected (`cauldron ripples reject`) is kept, and `get` answers it
  marked `rejected: true`; `nodes`, `search` and `region`'s members leave it out and say
  how many they left out in `rejected_hidden`; --include-rejected lists them too.
  `--pool <p>` names another pool than `ws`.

  --env and --cauldron may be left out inside a project linked with `cauldron link`: the
  nearest `.cauldron` marker file in the current directory or a parent names both. A flag
  or variable ($CAULDRON_ENV, $ENV, $CAULDRON_CAULDRON) always wins over the marker. When
  the marker supplied either value, stderr says so and the JSON carries
  `resolved_from: \"marker:<path>\"`. A write (import without --dry-run, recover) refuses
  to take its target from a marker that is tracked by git or owned by another user: the
  marker is per-machine and belongs in .gitignore (`cauldron link help`).
";

/// `cauldron soup <verb> …`.
pub(crate) fn verb_soup(args: &[String]) -> Result<i32> {
    match args.first().map(String::as_str) {
        Some("help") | Some("-h") | Some("--help") => {
            print!("{USAGE}");
            Ok(0)
        }
        Some("import") => import(&args[1..]),
        Some("import-corpus") => import_corpus(&args[1..]),
        Some("regions") => regions(&args[1..]),
        Some("region") => region(&args[1..]),
        Some("nodes") => nodes(&args[1..]),
        Some("search") => search(&args[1..]),
        Some("get") => get(&args[1..]),
        Some("recover") => recover(&args[1..]),
        Some("dump") => dump(&args[1..]),
        Some("dumps") => dumps(&args[1..]),
        Some("dump-get") => dump_get(&args[1..]),
        Some("dump-remove") => dump_remove(&args[1..]),
        Some("approve") => approve(&args[1..]),
        Some("claim") => claim(&args[1..]),
        Some("link") => link(&args[1..], false),
        Some("unlink") => link(&args[1..], true),
        Some("cite") => cite(&args[1..]),
        Some("changes") => changes(&args[1..]),
        Some("grammar") => crate::grammar::verb_grammar(&args[1..]),
        None => {
            eprint!("{USAGE}");
            Ok(2)
        }
        Some(other) => {
            eprintln!(
                "cauldron soup: no verb `{other}` (import | import-corpus | recover | regions | region | nodes | search | get | claim | link | unlink | cite | changes | grammar | dump | dumps | dump-get | dump-remove | approve | help)"
            );
            Ok(2)
        }
    }
}

#[derive(Default)]
struct Args {
    env: Option<String>,
    cauldron: Option<String>,
    pool: Option<String>,
    agent: Option<String>,
    max_chars: Option<usize>,
    region: Option<String>,
    node_type: Option<String>,
    state: Option<String>,
    limit: Option<usize>,
    depth: Option<usize>,
    dry_run: bool,
    remint: bool,
    allow_duplicate: bool,
    include_superseded: bool,
    edges_in: bool,
    include_rejected: bool,
    review: Option<String>,
    title: Option<String>,
    source: Option<String>,
    kind: Option<String>,
    offset: Option<usize>,
    raw: bool,
    yes: bool,
    unapprove: bool,
    abandon_merge: bool,
    abandon_undo: bool,
    lose_undo: bool,
    set_aside_later_writes: bool,
    keep_partial_merge: bool,
    settle_merge: Option<String>,
    view: Option<String>,
    turn_offset: Option<usize>,
    wait_secs: Option<usize>,
    release: bool,
    note: Option<String>,
    add: Option<String>,
    remove: Option<String>,
    since: Option<String>,
    wait: Option<usize>,
    pos: Vec<String>,
}

fn parse(verb: &str, args: &[String], allow: &[&str]) -> Result<Args> {
    let mut a = Args::default();
    let mut it = args.iter();
    let value = |it: &mut std::slice::Iter<'_, String>, flag: &str| -> Result<String> {
        it.next().cloned().with_context(|| format!("`cauldron soup {verb} {flag}` needs a value"))
    };
    while let Some(s) = it.next() {
        let (flag, inline) = match s.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (s.as_str(), None),
        };
        // A lone `-` is stdin (`soup dump -`), not an option.
        if flag.starts_with('-') && flag != "-" && !allow.contains(&flag) {
            bail!("unknown option `{s}` for `cauldron soup {verb}`");
        }
        let take = |it: &mut std::slice::Iter<'_, String>| -> Result<String> {
            match &inline {
                Some(v) => Ok(v.clone()),
                None => value(it, flag),
            }
        };
        match flag {
            "--env" => a.env = Some(take(&mut it)?),
            "--cauldron" | "-c" => a.cauldron = Some(take(&mut it)?),
            "--pool" => a.pool = Some(soup_door::reads::checked_pool(&take(&mut it)?)?),
            "--agent" => a.agent = Some(take(&mut it)?),
            "--max-chars" => a.max_chars = Some(count(flag, &take(&mut it)?)?),
            "--limit" => a.limit = Some(count(flag, &take(&mut it)?)?),
            "--depth" => a.depth = Some(count(flag, &take(&mut it)?)?),
            "--region" => a.region = Some(take(&mut it)?),
            "--type" => a.node_type = Some(take(&mut it)?),
            "--state" => a.state = Some(take(&mut it)?),
            "--dry-run" => a.dry_run = true,
            "--remint" => a.remint = true,
            "--allow-duplicate" => a.allow_duplicate = true,
            "--include-superseded" => a.include_superseded = true,
            "--edges-in" => a.edges_in = true,
            "--include-rejected" => a.include_rejected = true,
            "--review" => a.review = Some(soup_door::reads::checked_review(&take(&mut it)?)?),
            "--title" => a.title = Some(take(&mut it)?),
            "--source" => a.source = Some(take(&mut it)?),
            "--kind" => a.kind = Some(take(&mut it)?),
            "--offset" => a.offset = Some(count(flag, &take(&mut it)?)?),
            "--raw" => a.raw = true,
            "--yes" => a.yes = true,
            "--unapprove" => a.unapprove = true,
            "--abandon-merge" => a.abandon_merge = true,
            "--abandon-undo" => a.abandon_undo = true,
            "--lose-undo" => a.lose_undo = true,
            "--set-aside-later-writes" => a.set_aside_later_writes = true,
            "--keep-partial-merge" => a.keep_partial_merge = true,
            "--settle-merge" => a.settle_merge = Some(take(&mut it)?),
            "--view" => a.view = Some(take(&mut it)?),
            "--turn-offset" => a.turn_offset = Some(count(flag, &take(&mut it)?)?),
            "--wait-secs" => a.wait_secs = Some(count(flag, &take(&mut it)?)?),
            "--release" => a.release = true,
            "--since" => a.since = Some(take(&mut it)?),
            "--wait" => a.wait = Some(count(flag, &take(&mut it)?)?),
            "--note" => a.note = Some(take(&mut it)?),
            "--add" => a.add = Some(take(&mut it)?),
            "--remove" => a.remove = Some(take(&mut it)?),
            _ => a.pos.push(s.clone()),
        }
    }
    Ok(a)
}

fn count(flag: &str, v: &str) -> Result<usize> {
    v.parse().with_context(|| format!("{flag} takes a count, not `{v}`"))
}

/// The environment, named (never the default — a forgotten `--env` must not
/// land in prod), and the named cauldron in it. Either may come from the
/// project marker (`.cauldron`, `cauldron link`) found from the current
/// directory, but only where no flag or variable names it
/// (`soup_door::marker`). When it does, stderr says so, the JSON the verb
/// prints says so (`resolved_from`), and a verb that `writes` refuses a
/// marker that is tracked by git or owned by another user.
fn target(verb: &str, a: &Args, writes: bool) -> Result<Target> {
    // The current directory is read only if the marker is needed: given both
    // --env and --cauldron, a verb works from a directory that was deleted.
    let label = format!("`cauldron soup {verb}`");
    let r = soup_door::marker::resolve(&label, None, a.env.as_deref(), a.cauldron.as_deref())?;
    if let Some(m) = &r.marker {
        eprintln!(
            "cauldron soup {verb}: cauldron `{}` (from {}) in environment `{}` (from {}); marker {}",
            r.cauldron,
            if r.cauldron_from.starts_with("the project marker") { "the project marker" } else { r.cauldron_from.as_str() },
            r.env,
            if r.env_from.starts_with("the project marker") { "the project marker" } else { r.env_from.as_str() },
            m.display()
        );
    }
    let t = soup_door::target::locate(&label, &r, writes)?;
    if let Some(from) = &t.resolved_from {
        let _ = RESOLVED_FROM.set(from.clone());
    }
    Ok(t)
}

/// Set by [`target`] when the project marker supplied the environment or the
/// cauldron: `marker:<path>`. Every JSON document the verb prints then
/// carries it as `resolved_from`, so a caller reading only stdout knows the
/// target was not named explicitly.
static RESOLVED_FROM: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn print(v: &Value) -> Result<()> {
    let mut v = v.clone();
    if let (Some(report), Some(obj)) = (CORPUS_REPORT.get(), v.as_object_mut()) {
        let mut report = report.clone();
        if let Some(map) = obj.get("id_map") {
            report["id_map"] = crate::corpus5::corpus_id_map(map);
        }
        obj.insert("corpus".to_string(), report);
    }
    if let (Some(from), Some(obj)) = (RESOLVED_FROM.get(), v.as_object_mut()) {
        obj.insert("resolved_from".to_string(), json!(from));
    }
    // A record's or a dump's text can hold characters a terminal acts on:
    // they are written as JSON escapes, which mean the same string.
    println!("{}", soup_door::review::json_for_terminal(&v));
    Ok(())
}

fn read_opts(a: &Args) -> ReadOpts {
    ReadOpts {
        pool: a.pool.clone(),
        max_chars: a.max_chars,
        limit: a.limit,
        depth: a.depth,
        region: a.region.clone(),
        node_type: a.node_type.clone(),
        state: a.state.clone(),
        include_superseded: a.include_superseded,
        include_rejected: a.include_rejected,
        review: a.review.clone(),
        edges_in: a.edges_in,
    }
}

// ------------------------------------------------------------------- import

fn import(args: &[String]) -> Result<i32> {
    let a = parse(
        "import",
        args,
        &["--env", "--cauldron", "-c", "--pool", "--agent", "--dry-run", "--remint", "--allow-duplicate"],
    )?;
    let [input] = a.pos.as_slice() else {
        bail!("`cauldron soup import` takes ONE file or directory (got {}); `cauldron soup help`", a.pos.len());
    };
    let records = soup_door::import::read_records(Path::new(input))?;
    import_records(&a, input, &records)
}

/// **`cauldron soup import-corpus`**: the older corpus format
/// (`crate::corpus5`), converted to workspace-v6 records and landed through
/// exactly the import above, always under `--remint`: the same door, all or
/// nothing, the batch-already-landed guard, the write refusals for a target
/// the project marker resolved, and the forwarding to a running `mcp`
/// service. Every JSON it prints carries `corpus`: what was carried, what was
/// not and why, and (when the ids hold) the old-id to new-id map.
fn import_corpus(args: &[String]) -> Result<i32> {
    let mut a = parse(
        "import-corpus",
        args,
        &["--env", "--cauldron", "-c", "--pool", "--agent", "--dry-run", "--allow-duplicate"],
    )?;
    let [input] = a.pos.as_slice() else {
        bail!("`cauldron soup import-corpus` takes ONE file or directory (got {}); `cauldron soup help`", a.pos.len());
    };
    let input = input.clone();
    let converted = crate::corpus5::convert_path(Path::new(&input))?;
    let nc = converted.report["not_carried"].as_array().map_or(0, Vec::len);
    if nc > 0 {
        eprintln!(
            "cauldron soup import-corpus: {nc} item(s) of the input are not carried (listed under corpus.not_carried, each with its reason)"
        );
    }
    let _ = CORPUS_REPORT.set(converted.report);
    a.remint = true;
    import_records(&a, &input, &converted.records)
}

/// Set by [`import_corpus`]: merged into the document [`print`] prints.
static CORPUS_REPORT: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

fn import_records(a: &Args, input: &str, records: &[Value]) -> Result<i32> {
    // What a read adds is dropped (and named in `ignored`), and the review
    // flags and ripple provenance are refused, here, before the batch can be
    // forwarded to a running service (which may be an older one that does
    // not do either).
    let (records, ignored) = soup_door::writeback::strip_read_only(records);
    soup_door::review::refuse_review_flags(&records)?;
    soup_door::review::refuse_ripple_provenance(&records)?;
    let t = target("import", a, !a.dry_run)?;
    let opts = ImportOptions {
        pool: a.pool.clone(),
        dry_run: a.dry_run,
        remint: a.remint,
        allow_duplicate: a.allow_duplicate,
        agent: a.agent.clone().unwrap_or_else(|| "outside-agent".to_string()),
        dispatch: format!(
            "cauldron soup import {}",
            Path::new(input).file_name().and_then(|n| n.to_str()).unwrap_or("records")
        ),
    };
    let outcome = match soup_door::import::import(&t, &records, &opts) {
        Err(e) if e.code == soup_door::Code::Locked && !opts.dry_run => match soup_door::lock::held_by_service(&t.cauldron) {
            Some(pid) => match forward(&t, &records, &opts, pid, &ignored)? {
                Forwarded::Done(rc) => return Ok(rc),
                // The service let the cauldron go between the lock and the
                // call (a revoke): the soup is free again, so import directly.
                Forwarded::Released => soup_door::import::import(&t, &records, &opts)?,
            },
            // Busy (another writer, a merge landing): the refusal with
            // when to try again, as JSON on stdout too.
            None => return refused_doc("import", e),
        },
        other => other?,
    };
    match outcome {
        ImportOutcome::Judged { mut doc, ok } => {
            soup_door::writeback::note_ignored(&mut doc, &ignored);
            print(&doc)?;
            Ok(if ok { 0 } else { 1 })
        }
        ImportOutcome::AlreadyLanded { mut doc, why } => {
            soup_door::writeback::note_ignored(&mut doc, &ignored);
            print(&doc)?;
            eprintln!("cauldron soup import: REFUSED (batch-already-landed): {why}");
            Ok(1)
        }
    }
}

enum Forwarded {
    /// Answered and printed; the exit code.
    Done(i32),
    /// The service no longer holds the cauldron.
    Released,
}

/// **The import, sent to the `mcp` service** that holds the soup (its pid
/// is in the writer lock): the same records as one `soup_put`, with
/// `remint` and `allow_duplicate` as given, under the same batch digest, so
/// the batch-already-landed guard sees the same batch whichever way it
/// landed. Printed in the import's own shape, plus `forwarded_to`. Only when
/// the service's `mcp` seam row is LIVE and names the pid the lock names.
fn forward(t: &Target, records: &[Value], o: &ImportOptions, pid: u32, ignored: &[Value]) -> Result<Forwarded> {
    let svc = crate::mcp_door::SERVICE;
    let holder = format!("the `{svc}` service (pid {pid})");
    let found = match crate::mcp_door::find(&t.env) {
        Ok(f) if f.pid == pid => f,
        Ok(f) => bail!(
            "REFUSED: {holder} holds cauldron `{}`'s soup, but the running `{svc}` service is pid {}. Nothing was \
             written. See `cauldron mcp status --env {}`.",
            t.cauldron.name,
            f.pid,
            t.env.name
        ),
        Err(why) => bail!(
            "REFUSED: {holder} holds cauldron `{}`'s soup, so this import would be sent to it, but {}. Nothing was \
             written. Try again in a moment, or see `cauldron mcp status --env {}`.",
            t.cauldron.name,
            why.say(),
            t.env.name
        ),
    };
    let pool = o.pool_name();
    if pool.as_str() != soup_door::reads::DEFAULT_POOL {
        bail!(
            "REFUSED: {holder} holds cauldron `{}`'s soup and writes only its `{}` pool, so an import into `{pool}` \
             cannot be sent to it. Nothing was written.",
            t.cauldron.name,
            soup_door::reads::DEFAULT_POOL
        );
    }
    let mut args = json!({
        "cauldron": t.cauldron.name,
        "records": records,
        "remint": o.remint,
        "allow_duplicate": o.allow_duplicate,
        "_ctx": {"client": "cli"},
    });
    if let Ok(cwd) = std::env::current_dir() {
        args["_ctx"]["cwd"] = json!(cwd.display().to_string());
    }
    // One request, all or nothing: a batch the door would not read is
    // refused here, before anything is sent, and never split.
    let size = crate::mcp_door::call_body("soup_put", args.clone()).len();
    if size > crate::mcp_door::BODY_LIMIT {
        let (c, e) = (&t.cauldron.name, &t.env.name);
        bail!(
            "REFUSED: {holder} holds cauldron `{c}`'s soup, so this import would be sent to it as one request, and it \
             is {} ({} records), over the {} the service reads in one request. It is not sent in parts, because an \
             import lands all of it or none of it. Nothing was written. Either split the file into imports under that \
             size (each then lands all or nothing on its own, and under --remint a record's edges can name only \
             labels in its own file or ids already in the soup), or close the cauldron to the service and import \
             directly: `cauldron mcp revoke {c} --env {e}` (grant it again afterwards) or `cauldron --env {e} stop \
             {svc}`.",
            crate::mcp_door::mib(size),
            records.len(),
            crate::mcp_door::mib(crate::mcp_door::BODY_LIMIT)
        );
    }
    eprintln!(
        "cauldron soup import: {holder} holds cauldron `{}`'s soup; the import is sent to it and lands through it",
        t.cauldron.name
    );
    let result = match crate::mcp_door::call(&t.env, found.door, "soup_put", args) {
        Ok(r) => r,
        Err(err) if crate::mcp_door::is_lost(&err) => bail!(
            "{holder} went away while it had the import ({err:#}): WHETHER IT LANDED IS NOT KNOWN (an import lands whole \
             or not at all). Run the same import again once the service is back, or once it is stopped: if the first \
             one landed, it is refused as batch-already-landed; `cauldron soup changes --since <cursor>` lists what \
             landed after a cursor you had."
        ),
        Err(err) => return Err(err).with_context(|| format!("sending the import to {holder}; nothing is known to have been written, check `cauldron soup nodes`")),
    };
    let via = json!({"service": svc, "pid": pid});
    if result["isError"] != json!(true) {
        let mut doc = result["structuredContent"].clone();
        if let Some(m) = doc.as_object_mut() {
            m.remove("resolved_from");
            m.remove("cursor");
            m.insert("forwarded_to".into(), via);
        }
        soup_door::writeback::note_ignored(&mut doc, ignored);
        print(&doc)?;
        return Ok(Forwarded::Done(0));
    }
    let refused = &result["structuredContent"]["refused"];
    let code = refused["code"].as_str().unwrap_or("");
    let message = refused["message"].as_str().unwrap_or("");
    match code {
        "records_refused" => {
            let mut doc = refused["detail"].clone();
            if let Some(m) = doc.as_object_mut() {
                m.insert("forwarded_to".into(), via);
            }
            soup_door::writeback::note_ignored(&mut doc, ignored);
            print(&doc)?;
            Ok(Forwarded::Done(1))
        }
        "batch_already_landed" => {
            let earlier = refused["detail"]["landed_before"].as_array().cloned().unwrap_or_default();
            if earlier.is_empty() {
                bail!("REFUSED ({svc} service, batch_already_landed): {message}");
            }
            let digest = soup_door::import::batch_digest(&pool, records);
            let ImportOutcome::AlreadyLanded { mut doc, why } = soup_door::import::already_landed(t, &pool, &digest, o, records.len(), earlier) else {
                unreachable!("already_landed answers AlreadyLanded")
            };
            doc["forwarded_to"] = via;
            print(&doc)?;
            eprintln!("cauldron soup import: REFUSED (batch-already-landed): {why}");
            Ok(Forwarded::Done(1))
        }
        "not_granted" | "locked" => Ok(Forwarded::Released),
        _ => bail!("REFUSED ({holder} answered `{code}`): {message}"),
    }
}

// ------------------------------------------------------------------ changes

/// **`cauldron soup changes [--since <cursor>] [--wait <secs>]`**: what
/// landed since a cursor (`soup_door::changes`, the answer `soup_changes`
/// gives), read through connections that cannot write. `--wait` waits up to
/// that many seconds for something to land after `--since`; this process
/// writes nothing, so it looks at the store twice a second while it waits.
fn changes(args: &[String]) -> Result<i32> {
    let a = parse("changes", args, &["--env", "--cauldron", "-c", "--since", "--wait", "--limit", "--max-chars"])?;
    if let Some(p) = a.pos.first() {
        bail!("`cauldron soup changes` takes no arguments (got `{p}`); give the cursor as --since");
    }
    let t = target("changes", &a, false)?;
    let wait = a.wait.unwrap_or(0) as u64;
    let doc = match (&a.since, wait) {
        (None, w) if w > 0 => bail!("`cauldron soup changes --wait` needs --since <cursor> (a cursor to wait past)"),
        (Some(since), w) if w > 0 => soup_door::changes::changes_waiting(
            &t,
            since,
            a.limit,
            std::time::Duration::from_secs(w),
            &soup_door::changes::Sleep,
            std::time::Duration::from_millis(500),
            &mut || Ok(()),
        )?,
        (since, _) => soup_door::changes::changes(&t, since.as_deref(), a.limit)?,
    };
    let doc = match a.max_chars {
        None => doc,
        Some(m) => {
            let mut d = soup_door::budget::budget(doc, soup_door::budget::clamp(Some(m)));
            soup_door::changes::after_budget(&t, &mut d);
            d
        }
    };
    print(&doc)?;
    Ok(0)
}

// -------------------------------------------------------------------- claim

/// **`cauldron soup claim <cluster> <region> [--release] [--note <text>]`**:
/// a cluster claims a region, or releases its claim
/// (`soup_door::clusters::claim`). Written directly when nothing holds the
/// soup; while the `mcp` service holds it, sent to the service as one
/// `soup_claim`, as `import` sends a `soup_put`.
fn claim(args: &[String]) -> Result<i32> {
    let a = parse("claim", args, &["--env", "--cauldron", "-c", "--release", "--note", "--agent"])?;
    let [cluster, region] = a.pos.as_slice() else {
        bail!("`cauldron soup claim` takes a cluster and a region (`cauldron soup claim CL-1 R-3`); got {} argument(s)", a.pos.len());
    };
    let t = target("claim", &a, true)?;
    let k = soup_door::clusters::Claim { cluster: cluster.clone(), region: region.clone(), release: a.release, note: a.note.clone() };
    let w = soup_door::clusters::Writer {
        thread: a.agent.clone().unwrap_or_else(|| "outside-agent".to_string()),
        dispatch: "cauldron soup claim".into(),
    };
    let doc = match soup_door::clusters::claim_direct(&t, &k, &w) {
        Err(e) if e.code == soup_door::Code::Locked => match soup_door::lock::held_by_service(&t.cauldron) {
            Some(pid) => return claim_forward(&t, &k, pid),
            None => return Err(e.into()),
        },
        other => other?,
    };
    print(&doc)?;
    Ok(0)
}

/// The claim, sent to the `mcp` service (pid `pid`) that holds the soup.
fn claim_forward(t: &Target, k: &soup_door::clusters::Claim, pid: u32) -> Result<i32> {
    let svc = crate::mcp_door::SERVICE;
    let holder = format!("the `{svc}` service (pid {pid})");
    let found = match crate::mcp_door::find(&t.env) {
        Ok(f) if f.pid == pid => f,
        Ok(f) => bail!("REFUSED: {holder} holds cauldron `{}`'s soup, but the running `{svc}` service is pid {}. Nothing was written.", t.cauldron.name, f.pid),
        Err(why) => bail!("REFUSED: {holder} holds cauldron `{}`'s soup, so this claim would be sent to it, but {}. Nothing was written.", t.cauldron.name, why.say()),
    };
    let mut args = json!({"cauldron": t.cauldron.name, "cluster": k.cluster, "region": k.region, "release": k.release, "_ctx": {"client": "cli"}});
    if let Some(n) = &k.note {
        args["note"] = json!(n);
    }
    eprintln!("cauldron soup claim: {holder} holds cauldron `{}`'s soup; the claim is sent to it and lands through it", t.cauldron.name);
    let result = crate::mcp_door::call(&t.env, found.door, "soup_claim", args)
        .with_context(|| format!("sending the claim to {holder}; nothing is known to have been written, check `cauldron soup get {}`", k.cluster))?;
    if result["isError"] != json!(true) {
        let mut doc = result["structuredContent"].clone();
        if let Some(m) = doc.as_object_mut() {
            m.remove("resolved_from");
            m.insert("forwarded_to".into(), json!({"service": svc, "pid": pid}));
        }
        print(&doc)?;
        return Ok(0);
    }
    let refused = &result["structuredContent"]["refused"];
    let code = refused["code"].as_str().unwrap_or("");
    let message = refused["message"].as_str().unwrap_or("");
    if code == "unsupported" {
        bail!(
            "REFUSED: {holder} is from before clusters (it answers `soup_claim` as unsupported), so it cannot write a claim. \
             Nothing was written. Upgrade it (`scripts/install --replace-running`), then claim again."
        );
    }
    bail!("REFUSED ({holder} answered `{code}`): {message}")
}

// -------------------------------------------------------- link, unlink, cite

/// **`cauldron soup link | unlink <from> <kind> <to>`**: one edge added or
/// removed (`soup_door::edges`). Written directly when nothing holds the
/// soup; while the `mcp` service holds it, sent to it as `soup_link` or
/// `soup_unlink`.
fn link(args: &[String], remove: bool) -> Result<i32> {
    let verb = if remove { "unlink" } else { "link" };
    let allow: &[&str] = if remove { &["--env", "--cauldron", "-c", "--agent"] } else { &["--env", "--cauldron", "-c", "--note", "--agent"] };
    let a = parse(verb, args, allow)?;
    let [from, kind, to] = a.pos.as_slice() else {
        bail!("`cauldron soup {verb}` takes a record, an edge kind and a record (`cauldron soup {verb} DEC-2 ref T-4`); got {} argument(s)", a.pos.len());
    };
    let t = target(verb, &a, true)?;
    let l = soup_door::edges::Link { from: from.clone(), kind: kind.clone(), to: to.clone(), note: a.note.clone() };
    let w = soup_door::clusters::Writer {
        thread: a.agent.clone().unwrap_or_else(|| "outside-agent".to_string()),
        dispatch: format!("cauldron soup {verb}"),
    };
    let links = vec![l.clone()];
    let done = soup_door::edges::direct(&t, verb, |h| if remove { soup_door::edges::unlink(h, &t, &links, &w) } else { soup_door::edges::link(h, &t, &links, &w) });
    let doc = match done {
        Err(e) if e.code == soup_door::Code::Locked => match soup_door::lock::held_by_service(&t.cauldron) {
            Some(pid) => {
                let mut args = json!({"cauldron": t.cauldron.name, "from": l.from, "kind": l.kind, "to": l.to});
                if let Some(n) = &l.note {
                    args["note"] = json!(n);
                }
                return forward_tool(&t, verb, if remove { "soup_unlink" } else { "soup_link" }, args, pid);
            }
            None => return Err(e.into()),
        },
        Err(e) => return refused_doc(verb, e),
        Ok(d) => d,
    };
    print(&doc)?;
    Ok(0)
}

/// **`cauldron soup cite <id> --add <json> | --remove <index-or-json>`**:
/// one entry of a record's `meta.cites` added or removed
/// (`soup_door::edges::cite`), directly or through the `mcp` service.
fn cite(args: &[String]) -> Result<i32> {
    let a = parse("cite", args, &["--env", "--cauldron", "-c", "--add", "--remove", "--agent"])?;
    let [id] = a.pos.as_slice() else {
        bail!("`cauldron soup cite` takes one record id and --add or --remove (`cauldron soup cite DEC-2 --add '{{\"kind\":\"url\",\"url\":\"…\"}}'`)");
    };
    let (op, sent) = match (&a.add, &a.remove) {
        (Some(j), None) => {
            let v: serde_json::Value = serde_json::from_str(j).with_context(|| format!("--add takes one citation as JSON, not `{j}`"))?;
            if !v.is_object() {
                bail!("--add takes one citation object, e.g. '{{\"kind\":\"url\",\"url\":\"…\"}}'");
            }
            (soup_door::edges::CiteOp::Add(v.clone()), json!({"add": v}))
        }
        (None, Some(r)) => match r.trim().parse::<usize>() {
            Ok(i) => (soup_door::edges::CiteOp::Remove(soup_door::edges::Which::Index(i)), json!({"remove": i})),
            Err(_) => match serde_json::from_str::<serde_json::Value>(r) {
                Ok(serde_json::Value::Object(m)) => (soup_door::edges::CiteOp::Remove(soup_door::edges::Which::Matching(m.clone())), json!({"remove": m})),
                _ => bail!("--remove takes a citation's index (from 0) or a JSON object its entry matches, not `{r}`"),
            },
        },
        (Some(_), Some(_)) => bail!("`cauldron soup cite` takes --add or --remove, not both"),
        (None, None) => bail!("`cauldron soup cite` needs --add <json> or --remove <index-or-json>"),
    };
    let t = target("cite", &a, true)?;
    let w = soup_door::clusters::Writer {
        thread: a.agent.clone().unwrap_or_else(|| "outside-agent".to_string()),
        dispatch: "cauldron soup cite".into(),
    };
    let doc = match soup_door::edges::direct(&t, "cite", |h| soup_door::edges::cite(h, &t, id, &op, &w)) {
        Err(e) if e.code == soup_door::Code::Locked => match soup_door::lock::held_by_service(&t.cauldron) {
            Some(pid) => {
                let mut args = sent;
                args["cauldron"] = json!(t.cauldron.name);
                args["id"] = json!(id);
                return forward_tool(&t, "cite", "soup_cite", args, pid);
            }
            None => return Err(e.into()),
        },
        Err(e) => return refused_doc("cite", e),
        Ok(d) => d,
    };
    print(&doc)?;
    Ok(0)
}

/// A refusal that carries facts (`links_refused`): the facts on stdout as
/// JSON, the sentence on stderr, exit 1. Any other refusal as usual.
fn refused_doc(verb: &str, e: soup_door::Error) -> Result<i32> {
    match &e.detail {
        Some(d) => {
            print(d)?;
            eprintln!("cauldron soup {verb}: REFUSED ({}): {}", e.code, e.message);
            Ok(1)
        }
        None => Err(e.into()),
    }
}

/// An act sent to the `mcp` service (pid `pid`) that holds the soup, as
/// the tool `tool`; its answer printed as the direct write prints it.
fn forward_tool(t: &Target, verb: &str, tool: &str, mut args: serde_json::Value, pid: u32) -> Result<i32> {
    let svc = crate::mcp_door::SERVICE;
    let holder = format!("the `{svc}` service (pid {pid})");
    let found = match crate::mcp_door::find(&t.env) {
        Ok(f) if f.pid == pid => f,
        Ok(f) => bail!("REFUSED: {holder} holds cauldron `{}`'s soup, but the running `{svc}` service is pid {}. Nothing was written.", t.cauldron.name, f.pid),
        Err(why) => bail!("REFUSED: {holder} holds cauldron `{}`'s soup, so this would be sent to it, but {}. Nothing was written.", t.cauldron.name, why.say()),
    };
    args["_ctx"] = json!({"client": "cli"});
    eprintln!("cauldron soup {verb}: {holder} holds cauldron `{}`'s soup; this is sent to it and lands through it", t.cauldron.name);
    let result = crate::mcp_door::call(&t.env, found.door, tool, args)
        .with_context(|| format!("sending `{tool}` to {holder}; nothing is known to have been written, check `cauldron soup changes`"))?;
    let refused = &result["structuredContent"]["refused"];
    if result["isError"] != json!(true) {
        let mut doc = result["structuredContent"].clone();
        if let Some(m) = doc.as_object_mut() {
            m.remove("resolved_from");
            m.insert("forwarded_to".into(), json!({"service": svc, "pid": pid}));
        }
        print(&doc)?;
        return Ok(0);
    }
    let code = refused["code"].as_str().unwrap_or("");
    let message = refused["message"].as_str().unwrap_or("");
    if code == "-32602" || message.contains("unknown tool") || code == "unknown_method" {
        bail!("REFUSED: {holder} does not know `{tool}` (it is older than this cauldron). Nothing was written. Upgrade it (`scripts/install --replace-running`).");
    }
    if !refused["detail"].is_null() {
        print(&refused["detail"])?;
        eprintln!("cauldron soup {verb}: REFUSED ({holder} answered `{code}`): {message}");
        return Ok(1);
    }
    bail!("REFUSED ({holder} answered `{code}`): {message}")
}

// ------------------------------------------------------------------ recover

/// **`cauldron soup recover`**: settle a soup an interrupted import left
/// not at rest (`soup_door::recover`). Prints the outcome as JSON on stdout
/// and in words on stderr.
fn recover(args: &[String]) -> Result<i32> {
    let a = parse("recover", args, &["--env", "--cauldron", "-c", "--abandon-merge", "--abandon-undo", "--lose-undo", "--set-aside-later-writes", "--keep-partial-merge", "--settle-merge"])?;
    if !a.pos.is_empty() {
        bail!("`cauldron soup recover` takes no arguments (got `{}`); `cauldron soup help`", a.pos.join(" "));
    }
    let t = target("recover", &a, true)?;
    if (a.lose_undo || a.set_aside_later_writes) && !a.abandon_undo {
        bail!("`--lose-undo` and `--set-aside-later-writes` go with `--abandon-undo`; `cauldron soup help`");
    }
    if a.keep_partial_merge && !a.abandon_merge {
        bail!("`--keep-partial-merge` goes with `--abandon-merge`; `cauldron soup help`");
    }
    let report = soup_door::recover::recover_abandoning(
        &t,
        soup_door::recover::Abandon {
            merge: a.abandon_merge,
            keep_partial_merge: a.keep_partial_merge,
            undo: a.abandon_undo,
            undo_opts: soup_door::merge::AbandonUndo { lose_undo: a.lose_undo, set_aside_later_writes: a.set_aside_later_writes },
            at_terminal: crate::review::at_terminal(),
            var: std::env::var(env::owner_acts::VAR).ok(),
            settle: a.settle_merge.clone(),
        },
    )?;
    eprintln!("cauldron soup recover: {}", soup_door::recover::say(&report.doc));
    print(&report.doc)?;
    Ok(if report.settled { 0 } else { 1 })
}

// -------------------------------------------------------------------- reads

fn regions(args: &[String]) -> Result<i32> {
    let a = parse("regions", args, &["--env", "--cauldron", "-c", "--pool", "--max-chars"])?;
    if let Some(p) = a.pos.first() {
        bail!("`cauldron soup regions` takes no ids (got `{p}`); `soup get` reads nodes");
    }
    let t = target("regions", &a, false)?;
    print(&soup_door::reads::regions(&t, &read_opts(&a))?)?;
    Ok(0)
}

fn get(args: &[String]) -> Result<i32> {
    let a = parse("get", args, &["--env", "--cauldron", "-c", "--pool", "--max-chars", "--edges-in"])?;
    if a.pos.is_empty() {
        bail!("`cauldron soup get` needs at least one id");
    }
    let t = target("get", &a, false)?;
    let doc = soup_door::reads::get(&t, &a.pos, &read_opts(&a))?;
    print(&doc)?;
    Ok(if doc.get("missing").is_none() { 0 } else { 1 })
}

fn nodes(args: &[String]) -> Result<i32> {
    let a = parse(
        "nodes",
        args,
        &["--env", "--cauldron", "-c", "--pool", "--region", "--type", "--state", "--limit", "--max-chars", "--include-superseded", "--include-rejected", "--review"],
    )?;
    if let Some(p) = a.pos.first() {
        bail!("`cauldron soup nodes` takes no ids (got `{p}`); `soup get` reads nodes by id");
    }
    let t = target("nodes", &a, false)?;
    print(&soup_door::reads::nodes(&t, &read_opts(&a))?)?;
    Ok(0)
}

fn search(args: &[String]) -> Result<i32> {
    let a = parse(
        "search",
        args,
        &["--env", "--cauldron", "-c", "--pool", "--region", "--type", "--limit", "--max-chars", "--include-superseded", "--include-rejected", "--review"],
    )?;
    let query = a.pos.join(" ");
    if soup_door::reads::search_terms(&query).is_empty() {
        bail!("`cauldron soup search` needs a query with at least one word");
    }
    let t = target("search", &a, false)?;
    print(&soup_door::reads::search(&t, &query, &read_opts(&a))?)?;
    Ok(0)
}

fn region(args: &[String]) -> Result<i32> {
    let a = parse("region", args, &["--env", "--cauldron", "-c", "--pool", "--depth", "--limit", "--max-chars", "--include-superseded", "--include-rejected"])?;
    let [id] = a.pos.as_slice() else {
        bail!("`cauldron soup region` takes ONE region id (got {}); `cauldron soup regions` lists them", a.pos.len());
    };
    let t = target("region", &a, false)?;
    print(&soup_door::reads::region(&t, id, &read_opts(&a))?)?;
    Ok(0)
}

// -------------------------------------------------------------------- dumps

/// **`cauldron soup dump <file|->`**: keep its text as a dump.
fn dump(args: &[String]) -> Result<i32> {
    let a = parse("dump", args, &["--env", "--cauldron", "-c", "--title", "--source", "--wait-secs"])?;
    if a.wait_secs.is_some_and(|w| w as u64 > soup_door::dumps::WAIT_MAX_SECS) {
        bail!("`--wait-secs` is 0 to {}", soup_door::dumps::WAIT_MAX_SECS);
    }
    let [input] = a.pos.as_slice() else {
        bail!("`cauldron soup dump` takes ONE file, or - for stdin (got {}); `cauldron soup help`", a.pos.len());
    };
    // Read one byte past the limit, so an oversized input is known without
    // reading all of it.
    let limit = soup_door::dumps::TEXT_LIMIT as u64 + 1;
    let mut bytes = Vec::new();
    {
        use std::io::Read;
        if input == "-" {
            std::io::stdin().lock().take(limit).read_to_end(&mut bytes).context("reading stdin")?;
        } else {
            std::fs::File::open(input).with_context(|| format!("opening {input}"))?.take(limit).read_to_end(&mut bytes).with_context(|| format!("reading {input}"))?;
        }
    }
    if bytes.len() as u64 >= limit {
        bail!(
            "REFUSED (too_large): {input} is over the {} bytes (1 MiB) one dump holds. Nothing was written. Split it into several dumps.",
            soup_door::dumps::TEXT_LIMIT
        );
    }
    let text = String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("REFUSED: {input} is not UTF-8 text; a dump holds text. Nothing was written."))?;
    soup_door::dumps::check_text(&text)?;
    soup_door::dumps::check_extras(a.title.as_deref(), a.source.as_deref(), None, &[])?;
    let t = target("dump", &a, true)?;
    match soup_door::dumps::Store::open(&t.cauldron, &format!("cli {}", std::process::id())) {
        Ok(store) => {
            let r = dump_direct(&t, &store, text, &a)?;
            drop(store);
            print(&waited(&t, r, &a)?)?;
            Ok(0)
        }
        Err(e) if e.code == soup_door::Code::Locked => match soup_door::dumps::held_by_service(&t.cauldron) {
            Some(pid) => match dump_forward(&t, &text, &a, pid)? {
                Some(rc) => Ok(rc),
                // The service let the store go between the lock and the call.
                None => {
                    let store = soup_door::dumps::Store::open(&t.cauldron, &format!("cli {}", std::process::id()))?;
                    let r = dump_direct(&t, &store, text, &a)?;
                    drop(store);
                    print(&waited(&t, r, &a)?)?;
                    Ok(0)
                }
            },
            None => Err(e.into()),
        },
        Err(e) => Err(e.into()),
    }
}

fn dump_direct(t: &Target, store: &soup_door::dumps::Store, text: String, a: &Args) -> Result<Value> {
    use std::io::Write;
    let mut w = store.blob(soup_door::dumps::Kind::Text)?;
    w.write_all(text.as_bytes()).context("writing the dump's bytes")?;
    let by = soup_door::dumps::By {
        thread: "cli".into(),
        dispatch: "cauldron soup dump".into(),
        instrument: "cauldron".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    };
    let make = |blob: &soup_door::dumps::Blob| {
        Ok(soup_door::dumps::NewDump {
            env: t.env.name.clone(),
            content: soup_door::dumps::Content::Text { text, from: a.source.clone() },
            blob: blob.clone(),
            title: a.title.clone(),
            cite: None,
            refs: vec![],
            client: "cli".into(),
            session: None,
        })
    };
    let (d, landed) = store.commit(w, || Ok(()), make, &by, |e| e)?;
    let mut r = soup_door::dumps::receipt(&t.cauldron.name, &t.env.name, &d, &landed);
    let mut auto = env::ripples_auto::receipt(&t.env, &t.cauldron.name, &landed.id, d.kind().as_str());
    if auto["takes_this_dump"] == json!(true) {
        // Written here, no service told: the ripples service's scan finds it.
        auto["picked_up"] = json!("written straight to the store, so the ripples service finds it at its next scan (within a minute) if it is running");
    }
    r["automatic"] = auto;
    Ok(r)
}

/// `--wait-secs`: the receipt with what automatic processing made of the
/// dump within the wait (`processing`), when it takes it.
fn waited(t: &Target, mut r: Value, a: &Args) -> Result<Value> {
    let wait = a.wait_secs.unwrap_or(0) as u64;
    if wait > 0 && r["automatic"]["takes_this_dump"] == json!(true) {
        if let Some(id) = r["dump_id"].as_str().map(str::to_string) {
            r["processing"] = soup_door::dumps::wait_processing(&t.cauldron, &id, wait)?;
        }
    }
    Ok(r)
}

/// The dump, sent to the `mcp` service that holds the store as one
/// `soup_dump`. `None`: the service no longer holds it (dump directly).
fn dump_forward(t: &Target, text: &str, a: &Args, pid: u32) -> Result<Option<i32>> {
    let svc = crate::mcp_door::SERVICE;
    let holder = format!("the `{svc}` service (pid {pid})");
    let found = match crate::mcp_door::find(&t.env) {
        Ok(f) if f.pid == pid => f,
        Ok(f) => bail!(
            "REFUSED: {holder} holds cauldron `{}`'s dumps store, but the running `{svc}` service is pid {}. Nothing was written. See `cauldron mcp status --env {}`.",
            t.cauldron.name,
            f.pid,
            t.env.name
        ),
        Err(why) => bail!(
            "REFUSED: {holder} holds cauldron `{}`'s dumps store, so this dump would be sent to it, but {}. Nothing was written. Try again in a moment.",
            t.cauldron.name,
            why.say()
        ),
    };
    let mut args = json!({"cauldron": t.cauldron.name, "text": text, "_ctx": {"client": "cli"}});
    if let Some(x) = &a.title {
        args["title"] = json!(x);
    }
    if let Some(x) = &a.source {
        args["source"] = json!(x);
    }
    if let Some(w) = a.wait_secs {
        args["wait_secs"] = json!(w);
    }
    if let Ok(cwd) = std::env::current_dir() {
        args["_ctx"]["cwd"] = json!(cwd.display().to_string());
    }
    eprintln!("cauldron soup dump: {holder} holds cauldron `{}`'s dumps store; the dump is sent to it and lands through it", t.cauldron.name);
    let result = crate::mcp_door::call(&t.env, found.door, "soup_dump", args)
        .with_context(|| format!("sending the dump to {holder}; nothing is known to have been written, check `cauldron soup dumps`"))?;
    if result["isError"] != json!(true) {
        let mut doc = result["structuredContent"].clone();
        if let Some(m) = doc.as_object_mut() {
            m.remove("resolved_from");
            m.remove("cursor");
            m.insert("forwarded_to".into(), json!({"service": svc, "pid": pid}));
        }
        print(&doc)?;
        return Ok(Some(0));
    }
    let refused = &result["structuredContent"]["refused"];
    let code = refused["code"].as_str().unwrap_or("");
    match code {
        "not_granted" | "locked" => Ok(None),
        _ => bail!("REFUSED ({holder} answered `{code}`): {}", refused["message"].as_str().unwrap_or("")),
    }
}

/// **`cauldron soup approve <id>... --env <e> --cauldron <c> [--unapprove]`**:
/// the owner's approval of a ripple's records, sent to the running `mcp`
/// service's `DECIDE` seam verb with the approval key it minted
/// (`approve.secret`), as `ripples approve` sends it
/// (`crate::review::send`). Refused unless stdin and stdout are an interactive
/// terminal or the owner opened `approve` (`crate::owner_acts::act`),
/// checked before the key is read, as `cauldron mcp grant` is.
fn approve(args: &[String]) -> Result<i32> {
    let a = parse("approve", args, &["--env", "--cauldron", "-c", "--unapprove"])?;
    if a.pos.is_empty() {
        bail!("`cauldron soup approve` takes one or more record ids (`cauldron soup nodes --review pending` lists them)");
    }
    let (Some(env_name), Some(cauldron)) = (a.env.as_deref(), a.cauldron.as_deref()) else {
        bail!(
            "REFUSED: `cauldron soup approve` takes the environment and the cauldron only as flags: --env <env> --cauldron \
             <name>. Nothing was written."
        );
    };
    let e = crate::mcp::environment(Some(env_name))?;
    let word = if a.unapprove { "unapprove" } else { "approve" };
    let command = format!(
        "cauldron soup approve {} --env {} --cauldron {cauldron}{}",
        a.pos.join(" "),
        e.name,
        if a.unapprove { " --unapprove" } else { "" }
    );
    // Checked before anything is read or sent, so the command the refusal
    // names is one that would work.
    let allowed = crate::owner_acts::act(
        env::owner_acts::Act::Approve,
        &e,
        cauldron,
        &format!(
            "REFUSED: approving a ripple's records is the owner's act at a terminal. `cauldron soup approve` runs only with \
             its input and output on an interactive terminal, unless the owner has let agents approve (owner_acts), so an \
             agent, a ripple or a script cannot {word} records by itself. Nothing was written. Ask the user to run, in a \
             terminal on this machine:\n    {command}"
        ),
    )?;
    let body = serde_json::json!({"action": word, "ids": a.pos, "from": "cauldron soup approve"});
    let v = crate::review::send(&e, cauldron, body, allowed.by())?;
    print(&v)?;
    Ok(if v["ok"] == serde_json::json!(true) { 0 } else { 1 })
}

/// **`cauldron soup dump-remove <id> --env <e> --cauldron <c> [--yes]`**:
/// remove one dump's content for good (`soup_door::dumps::remove`). CLI
/// only: never forwarded to the `mcp` service, which is refused instead,
/// because the erasure needs the store's databases to itself (it rewrites
/// and compacts them) and the service keeps them open while it holds them.
fn dump_remove(args: &[String]) -> Result<i32> {
    let a = parse("dump-remove", args, &["--env", "--cauldron", "-c", "--yes"])?;
    let [id] = a.pos.as_slice() else {
        bail!("`cauldron soup dump-remove` takes ONE dump id (got {}); `cauldron soup dumps` lists them", a.pos.len());
    };
    if a.env.is_none() || a.cauldron.is_none() {
        bail!(
            "REFUSED: `cauldron soup dump-remove` cannot be undone, so it takes the environment and the cauldron only as \
             flags: --env <env> --cauldron <name>. Nothing was changed."
        );
    }
    let t = target("dump-remove", &a, true)?;
    let by = soup_door::dumps::By {
        thread: "cli".into(),
        dispatch: "cauldron soup dump-remove".into(),
        instrument: "cauldron".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    };
    match soup_door::dumps::remove(&t.cauldron, id, a.yes, &by) {
        Ok(doc) => {
            print(&doc)?;
            Ok(if a.yes { 0 } else { 2 })
        }
        Err(e) if e.code == soup_door::Code::Locked => match soup_door::dumps::held_by_service(&t.cauldron) {
            Some(pid) => bail!(
                "REFUSED: the `{}` service (pid {pid}) holds cauldron `{}`'s dumps store, and a removal needs the store to \
                 itself (it rewrites and compacts its databases), so it is not sent to the service. Nothing was changed. \
                 Revoke the grant (`cauldron mcp revoke {} --env {}`) or stop the service (`cauldron --env {} stop {}`), \
                 run this again, then grant again.",
                crate::mcp_door::SERVICE,
                t.cauldron.name,
                t.cauldron.name,
                t.env.name,
                t.env.name,
                crate::mcp_door::SERVICE
            ),
            None => Err(e.into()),
        },
        Err(e) => Err(e.into()),
    }
}

fn dumps(args: &[String]) -> Result<i32> {
    let a = parse("dumps", args, &["--env", "--cauldron", "-c", "--state", "--kind", "--limit"])?;
    if let Some(p) = a.pos.first() {
        bail!("`cauldron soup dumps` takes no arguments (got `{p}`); `soup dump-get` reads one");
    }
    let t = target("dumps", &a, false)?;
    let o = soup_door::dumps::ListOpts { state: a.state.clone(), kind: a.kind.clone(), limit: a.limit };
    print(&soup_door::dumps::list(&t.cauldron, &o)?)?;
    Ok(0)
}

fn dump_get(args: &[String]) -> Result<i32> {
    let a = parse("dump-get", args, &["--env", "--cauldron", "-c", "--offset", "--max-chars", "--raw", "--view", "--turn-offset"])?;
    let [id] = a.pos.as_slice() else {
        bail!("`cauldron soup dump-get` takes ONE dump id (got {}); `cauldron soup dumps` lists them", a.pos.len());
    };
    let text_view = match a.view.as_deref() {
        None | Some("raw") => false,
        Some("text") => true,
        Some(v) => bail!("--view is `raw` (the default) or `text`, not `{v}`"),
    };
    if text_view && a.raw {
        bail!("--raw prints the content verbatim; it does not go with --view text");
    }
    if a.turn_offset.is_some() && !text_view {
        bail!("--turn-offset goes with --view text (it is a page's `next_turn_offset`)");
    }
    if !a.raw {
        soup_door::dumps::check_page_max(a.max_chars)?;
    }
    let t = target("dump-get", &a, false)?;
    if text_view {
        let max = soup_door::budget::clamp(a.max_chars);
        let off = a.offset.unwrap_or(0) as u64;
        print(&soup_door::dumps::get_text(&t.cauldron, id, off, a.turn_offset.unwrap_or(0) as u64, max)?)?;
        return Ok(0);
    }
    if !a.raw {
        let max = soup_door::budget::clamp(a.max_chars);
        print(&soup_door::dumps::get(&t.cauldron, id, a.offset.unwrap_or(0) as u64, max)?)?;
        return Ok(0);
    }
    // The whole content, verbatim, to a file or a pipe. To a terminal, the
    // characters a terminal would act on (escape sequences, bidirectional
    // overrides, invisible characters) are shown escaped instead, lines kept.
    use std::io::{IsTerminal, Read, Write};
    let mut content = soup_door::dumps::open_content(&t.cauldron, id)?;
    let mut out = std::io::stdout().lock();
    if std::io::stdout().is_terminal() {
        let mut bytes = Vec::new();
        (&mut content).take(u64::MAX).read_to_end(&mut bytes)?;
        out.write_all(soup_door::review::shown(&String::from_utf8_lossy(&bytes), true).as_bytes())?;
    } else {
        std::io::copy(&mut (&mut content).take(u64::MAX), &mut out)?;
    }
    out.flush()?;
    Ok(0)
}
