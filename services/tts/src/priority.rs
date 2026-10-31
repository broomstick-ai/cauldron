//! The priority scale, published as numbers because the face that uses it is
//! another process and cannot link a constant.
//!
//! | value | meaning |
//! |-------|---------|
//! | `0`   | [`DEFAULT`] — ordinary content; bulk warming lives here |
//! | `1`   | [`SYSTEM`] — interface chrome, heard on every keypress |
//! | `2..` | the look-ahead ladder, [`look_ahead`] |
//!
//! The operator's own example: *"The default priority is zero… the system ones
//! to, like, a priority of one. And then all of the look-ahead messages can be
//! six down to two."* `0` and `1` are per-cache DEFAULTS (`priority` in a
//! `tts.toml` stanza); an item that states a priority replaces its cache's
//! default rather than adding to it, which is why the ladder starts at 2.
//!
//! A caller somebody is waiting on is not expressed on this scale at all: the
//! queue adds [`crate::queue::INTERACTIVE`] for a blocked `tts.request`.

pub const DEFAULT: i64 = 0;
pub const SYSTEM: i64 = 1;

/// How many messages ahead the face warms; the ladder has this many rungs.
pub const LOOK_AHEAD_DEPTH: u32 = 5;

/// The furthest rung, one above [`SYSTEM`].
pub const LADDER_FLOOR: i64 = SYSTEM + 1;

/// Priority for a message `distance` ahead of the one being heard. A formula
/// rather than a table ("that's just an example strategy"): the nearest rung
/// is highest, the furthest is [`LADDER_FLOOR`], and anything beyond the
/// window is clamped to the floor rather than sinking below content.
///
/// ```
/// assert_eq!((1..=5).map(tts::priority::look_ahead).collect::<Vec<_>>(), vec![6, 5, 4, 3, 2]);
/// ```
pub fn look_ahead(distance: u32) -> i64 {
    LADDER_FLOOR + LOOK_AHEAD_DEPTH.saturating_sub(distance) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ladder_is_six_down_to_two_and_clamps_at_the_floor() {
        assert_eq!((1..=LOOK_AHEAD_DEPTH).map(look_ahead).collect::<Vec<_>>(), vec![6, 5, 4, 3, 2]);
        assert_eq!(look_ahead(LOOK_AHEAD_DEPTH + 3), LADDER_FLOOR);
        assert_eq!(look_ahead(u32::MAX), LADDER_FLOOR);
        assert_eq!(look_ahead(0), look_ahead(1) + 1, "the playing message sits above the next");
    }

    #[test]
    fn every_rung_clears_both_defaults_and_nearer_is_higher() {
        for d in 1..=LOOK_AHEAD_DEPTH {
            assert!(look_ahead(d) > SYSTEM && look_ahead(d) > DEFAULT, "rung {d}");
            if d < LOOK_AHEAD_DEPTH {
                assert!(look_ahead(d) > look_ahead(d + 1));
            }
        }
    }
}
