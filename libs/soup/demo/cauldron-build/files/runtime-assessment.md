# Where the ripple runtime and the soup stand

- The ripple runtime (type one, two and three, the conductor, the run journal)
  lives on its own branch and can execute a dump end to end.
- The soup has two lines of work: the write side (the door, the initiator, the
  flag rule) and the event side (the log, the bus, the publisher). One branch
  reconciles both.
- The tranche lock does not exist yet on any branch, and neither does the
  lander, so no ripple can land a write in the soup today.

## What the first release carries

The new face, with the ledger, dumps and ripple trees shown from curated data in
the cauldron-build environment. The runtime merges into the trunk next week,
after the lock exists.
