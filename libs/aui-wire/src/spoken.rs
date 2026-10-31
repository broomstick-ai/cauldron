//! `Spoken` — TTS text modelled as a stable head plus volatile slots.
//!
//! The whole point is audio-cache survivability: a label like
//! "Unread. 2 threads" changes its count constantly, which would blow the
//! content-addressed AIFF cache on every change if cached as one blob. By
//! splitting into a stable head ("Unread.") plus a tiny set of volatile
//! fragments (the count "2", the word "threads"), each fragment is cached and
//! prewarmed independently, and the client concatenates cached clips at
//! playback time. The small prewarmed fragment set (digits + a handful of
//! words) means cache hits survive count churn.

use serde::{Deserialize, Serialize};

/// One volatile fragment of a spoken label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpokenSlot {
    /// A small integer (count). Rendered as its decimal string; the prewarmed
    /// digit set keeps these cheap to cache.
    Count(i64),
    /// An arbitrary text fragment (kept small and reused, e.g. "threads").
    Frag(String),
}

impl SpokenSlot {
    pub fn render(&self) -> String {
        match self {
            SpokenSlot::Count(n) => n.to_string(),
            SpokenSlot::Frag(s) => s.clone(),
        }
    }
}

/// Spoken text = stable head + volatile slots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spoken {
    pub head: String,
    #[serde(default)]
    pub slots: Vec<SpokenSlot>,
}

impl Spoken {
    /// A label with no volatile slots (the common case).
    pub fn plain(head: impl Into<String>) -> Self {
        Self {
            head: head.into(),
            slots: Vec::new(),
        }
    }

    /// A head plus slots.
    pub fn with_slots(head: impl Into<String>, slots: Vec<SpokenSlot>) -> Self {
        Self {
            head: head.into(),
            slots,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.head.trim().is_empty() && self.slots.is_empty()
    }

    /// The full one-line rendering (head + slots joined with spaces). Used for
    /// TUI display and for whole-utterance synthesis fallbacks.
    pub fn render(&self) -> String {
        let mut out = self.head.trim().to_string();
        for slot in &self.slots {
            let frag = slot.render();
            let frag = frag.trim();
            if frag.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(frag);
        }
        out
    }

    /// The individually-cacheable fragments, in playback order. The audio
    /// client ensures each fragment's clip is cached (fetching misses via
    /// `TtsBatch`) and plays them back-to-back. The prefetch walker collects
    /// these to warm the cache.
    pub fn fragments(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(1 + self.slots.len());
        let head = self.head.trim();
        if !head.is_empty() {
            out.push(head.to_string());
        }
        for slot in &self.slots {
            let frag = slot.render();
            let frag = frag.trim();
            if !frag.is_empty() {
                out.push(frag.to_string());
            }
        }
        out
    }
}

impl From<&str> for Spoken {
    fn from(s: &str) -> Self {
        Spoken::plain(s)
    }
}

impl From<String> for Spoken {
    fn from(s: String) -> Self {
        Spoken::plain(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_joins_head_and_slots() {
        let s = Spoken::with_slots(
            "Unread.",
            vec![SpokenSlot::Count(2), SpokenSlot::Frag("threads".into())],
        );
        assert_eq!(s.render(), "Unread. 2 threads");
        assert_eq!(s.fragments(), vec!["Unread.", "2", "threads"]);
    }

    #[test]
    fn plain_has_single_fragment() {
        let s = Spoken::plain("Options");
        assert_eq!(s.render(), "Options");
        assert_eq!(s.fragments(), vec!["Options"]);
    }
}
