# Roadmap

This is a plan, not a promise: items can move, and this file changes as they
do. Nothing below has a date. What each release actually contains is
recorded in [CHANGELOG.md](CHANGELOG.md).

## Shipped

- **0.1.2 and 0.1.3: the V1 interface.** Two tab bars, a first-run screen,
  the Ledger's first version, the account pool and its usage, secret human
  actions, `cauldron ctl`, and account polish.
- **0.1.4 and 0.1.5: a soup as a knowledge graph from Claude Code.** The
  published soup schema, `cauldron create` and `list`, the `cauldron soup`
  verbs, `cauldron link`, and the Claude Code plugin with its curator.
- **0.1.6: the MCP door**, a supervised service through which Claude Code
  sessions read, search and write the cauldrons their owner grants.
- **0.1.7: dumps**, raw text or a session transcript kept in a cauldron.
- **0.1.8: from install to a working session**, curating a dump into
  records by hand, `cauldron rename` and `remove`, a cauldron's data private
  to its owner.
- **0.1.9: removing a dump for good**, the MCP service hardened, and a
  schema for `--remint` batches (issue #17).
- **0.1.10: the first ripple, run by hand**: one dump into unreviewed
  records.
- **0.1.11: local inference**: the dump ripple on a model on this machine.
- **0.1.12: automatic ripples behind a setting**, off by default and turned
  on per cauldron by its owner at a terminal, with a secret scrubber.
- **0.1.13: the review queue**: the records a ripple wrote, grouped by dump,
  approved or rejected in bulk by the owner at a terminal.
- **0.1.14: region locks**: a ripple locks by region, never the whole
  soup: from before its model call until its records land it holds the few
  regions its dump clearly points at (often none), and it takes the regions
  its records go into for the write itself, so a write elsewhere in the
  soup lands meanwhile, a write into a held region is refused at once by
  name with a retry-after, and a region whose boundary changed under the
  ripple makes it plan again once, told what changed.
- **0.1.15: cauldron on an Apple silicon Mac without a Rust toolchain**:
  prebuilt, ad-hoc-signed arm64 binaries with checksums and an installer
  path that uses them (issue #22), a plugin MCP launcher that does not
  depend on Claude Code's `PATH`, and `cauldron soup import-corpus`, which
  brings records in the older corpus format into a cauldron's soup (issue
  #4).
- **0.1.16: clusters claiming regions** (issue #1): a cluster, an area of
  the surface with a name and a boundary, claims the regions of a soup it
  covers, in a second published schema (`cauldron@1`) beside the unchanged
  `workspace@6`; `soup_claim` and `cauldron soup claim`; a region lists the
  clusters that claim it; `import-corpus` brings an older corpus's clusters
  in with their claims; and `soup_changes` can wait for the next change
  (`wait_secs`), the polling half of issue #7.
- **0.1.17: the consumer interface as written** (issue #1): every tool in
  the interface table of issue #10 answers when called as the table writes
  it, a Claude Code project can have a server for one cauldron, who may act
  for the owner becomes the owner's setting (issue #25), and the prebuilt
  bundle carries local inference and the account helper scripts (issue
  #30).
- **0.1.18: installing and upgrading from a prebuilt bundle** (issues #30
  to #35): `install --replace-running` moves the threads daemon to the new
  build as it does the MCP service, so the account helpers it lays down
  are the ones the running daemon reads; account login runs on macOS's own
  bash and reports where it stalled; an account is named by its email
  address; and text can be selected and copied on every surface of the
  terminal interface.

## Next

- **Threads on a local model**, in a later release: the `inference`
  service stays the one owner of `llama-server`, the model registry and the
  memory budget, and the threads daemon leases a loaded model from it for a
  turn (load, renew, release), so one budget covers both. The chat face
  then offers a local model at thread creation, and an install with no
  Claude account can run threads.
- **Ripples on by default in a fresh install**, once the conditions hold:
  enough real dumps processed and reviewed with a reject rate the owner
  accepts, tokens per dump measured with no cap ever passed silently, the
  scrubber's corpus with no known-format miss, every kill and restart
  proven to land once, and `ripples off` documented and reversible. An
  upgrade would still leave it off.

## Open issues, not yet scheduled

- **Changes as events** (issue #7): a streaming subscription to what
  changed in a soup, and a history of which write changed what.
  (`soup_changes` can already wait for the next change.)
- **Ripples proposing clusters and claims**: a ripple writes neither today.
- **Ripples that split writing from reasoning** (issue #27): cheaper models
  write to the graph, and a stronger one reasons over issues,
  contradictions and work items to harmonize them. Until then a ripple's
  first call is Opus.
- **Merging cauldrons** (issue #9).
- **A summary page in the face** (issue #11).
- **Comparisons with two other tools**, as input for design: octomind's
  cross-session learning (issue #12) and Prospero's headless harness
  (issue #13).
- **Notarised binaries**, signed with a developer identity, so a browser
  download runs without removing the quarantine attribute.

## 0.2.0

Ripples and the soup engine: dumps set off ripples of every kind, the soup
engine is hardened, and the Ledger shows real ripple events. The 0.1.x
releases carry only the dump ripple.

## Further out

- **Inference adapters** on the threads daemon: Codex first, then others.
- **Local models** for the rest of cauldron's model work (0.1.11 runs only
  the dump ripple and its evaluation on one; threads on a local model are
  under Next).
- **Encrypted cauldrons**: local inference only, everything encrypted at
  rest.
- **Cloud and remote cauldrons**, and **mesh setups** of several machines.
