# Install path notes

- macOS only for the first release.
- No home directory is written into any script; everything resolves from the
  environment's root at run time.
- Binaries are signed ad hoc by default. A personal signing identity is never
  assumed.
- One script installs the face and the threads daemon. Other services are
  present but not started by default.
- The proof is an install as a fresh standard user that has never run cauldron.
