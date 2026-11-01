# Roadmap

This is the plan after the 0.1.3 release. It is a plan, not a promise: items
can move between releases, and this file changes as they do. After
0.2.0, releases come out weekly. What each release actually contains is
recorded in [CHANGELOG.md](CHANGELOG.md).

## V1

Shipped in 0.1.2 (see [CHANGELOG.md](CHANGELOG.md)):

- **Two tab bars:** a global Config bar (TUI, devices, providers, accounts),
  and an environment bar for Cauldron, Threads, Status, Permissions and
  Settings.
- **First run:** "+ create your first cauldron", then a cauldron with no
  cluster opens ready to name its first one.
- **Ledger, first version:** a time-ordered list of dumps, each expanding into
  its phases and the work under it, read from the environment's own data.
- **Tokens:** the real account pool and its usage pressure, under Status.
- **Inference:** Claude Code only. Other endpoints are shown greyed out until
  their adapters exist.
- **Secret human actions:** a masked input whose value is routed to the macOS
  keychain or an env file, and never into the soup.
- **Cleanup** of leftover names and states in the UI.
- **`cauldron ctl`:** send a dump, print one frame of the face, or read one
  soup node, from the command line.
- **Accounts:** the threads daemon's accounts under Config, masked; log in
  once from the face, then switch each account on or off per cauldron.

Shipped in 0.1.3: account polish (account settings read-only, with a notice,
when the install's key is wrong; clearer refusals; a 5-second connect timeout).

Coming in 0.2.0. Ripples and the soup engine arrive in 0.2.0:

- **Ripples live:** dumps set off ripples, and ripples write to the soup. The
  soup engine is hardened, and the Ledger shows real ripple events. (Not in
  0.1.0, 0.1.1, 0.1.2 or 0.1.3.)

## Weekly releases

Each weekly release covers one part of the system.

1. **Thread folding.** What you say in the Threads tab is folded back into the
   soup, and each fold appears in the Ledger.
2. **Stability core.** A database locking design, a guarantee that no message
   is ever dropped under load, and list updates sent as changes rather than
   whole lists.
3. **Tasks and Nodes views.** Tasks show the order of operations clearly, and
   Nodes show more kinds of things.
4. **Protoledger migration.** The ripples' record moves onto
   [protoledger](https://github.com/broomstick-ai/protoledger), a separate
   ledger project that cauldron already depends on.
5. **Messaging and structured output.** The messaging framework between agents,
   and structured output from agent turns.
6. **Inference adapters.** The threads daemon is refactored to support more than
   one inference adapter (see below).
7. **The `cauldron-claude` shim.** Claude Code's own interactive interface,
   running on a cauldron thread that is placed on the account pool.

## Further out

- **Inference adapters**, on the inference layer of the threads daemon:
  **Codex** first, then others.
- **Local models.** Running inference on models on your own machine.
- **Encrypted cauldrons.** A fully private, offline mode: local inference only,
  and everything encrypted at rest.
- **Cloud and remote cauldrons.** A cauldron that runs somewhere other than
  the machine you are sitting at.
- **Mesh setups.** Today a cauldron runs on a single mesh node. Later, several
  machines can work together as one mesh.
