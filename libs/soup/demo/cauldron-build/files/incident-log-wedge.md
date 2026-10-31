# Incident: the log wedge

What happened: a shape stored a list of rows inside a single cell. Every change
re-sent the whole window, frames backed up behind it, and every open face
stalled for seconds at a time.

The fix:
- A row carries a count and its children's ids, never their rows.
- Each change travels as the change: one item per frame.
- Bodies are hydrated by id, concurrently, visible first, off the render thread.
- Backpressure coalesces and never drops.

What it taught: a test that a shape is granular runs on every new or changed
shape, with no exceptions.
