//! The board's state and its key handling, with no terminal and no socket.
//!
//! [`App::key`] returns the [`Action`] the operator CONFIRMED, if any; the
//! event loop executes it. Every act goes through a confirm line first, as
//! every act on the Python board goes through a modal.

use chrono::{DateTime, Utc};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;

use super::action::Action;
use std::collections::BTreeSet;

use super::model::{project, wanted, Board, Fold, Tone, ACCOUNTS, NON_IDLE, SUMMARY, THREADS};

#[derive(Debug, Clone, PartialEq)]
pub enum LinkState {
    Connecting,
    Live,
    Offline(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Prompt {
    None,
    Confirm(Action),
    /// `S`: typing a percentage for this label.
    Share { label: String, buf: String },
}

pub struct App {
    pub accounts: Fold,
    pub non_idle: Fold,
    /// Keyed `threads` rows — parked entries only, for their reason.
    pub threads: Fold,
    /// Keyed `threads/summary` rows — names for roster threads and parents.
    pub summary: Fold,
    /// Every `(shape, key)` already asked for, so a key is subscribed once.
    pub requested: BTreeSet<(&'static str, String)>,
    pub now: DateTime<Utc>,
    /// The selected card's LABEL, so a card arriving or leaving does not move
    /// the selection onto another account under the operator's finger.
    pub selected: Option<String>,
    pub link: LinkState,
    pub note: Option<(String, Tone)>,
    pub prompt: Prompt,
    pub quit: bool,
    /// The daemon this board is drawing, shown in the header.
    pub endpoint: String,
}

impl App {
    pub fn new(endpoint: &str, now: DateTime<Utc>) -> Self {
        Self {
            accounts: Fold::new(ACCOUNTS),
            non_idle: Fold::new(NON_IDLE),
            threads: Fold::new(THREADS),
            summary: Fold::new(SUMMARY),
            requested: BTreeSet::new(),
            now,
            selected: None,
            link: LinkState::Connecting,
            note: None,
            prompt: Prompt::None,
            quit: false,
            endpoint: endpoint.to_string(),
        }
    }

    pub fn on_frame(&mut self, f: &Value) -> bool {
        let a = self.accounts.on_frame(f);
        let b = self.non_idle.on_frame(f);
        let c = self.threads.on_frame(f);
        let d = self.summary.on_frame(f);
        a || b || c || d
    }

    pub fn board(&self) -> Board {
        project(&self.accounts, &self.non_idle, &self.threads, &self.summary, self.now)
    }

    /// What account `label` is drawn as on this board: the one masker, told
    /// apart from a twin that masks the same (V1-ACCT).
    pub fn shown(&self, label: &str) -> String {
        crate::pool::model::shown_among(label, self.accounts.rows.keys().map(String::as_str))
    }

    /// The keyed subscribes the roster now calls for and nobody has sent.
    /// Marks them asked, so the caller sends each once.
    pub fn take_wanted(&mut self) -> Vec<(&'static str, String)> {
        wanted(&self.non_idle)
            .into_iter()
            .filter(|w| self.requested.insert(w.clone()))
            .collect()
    }

    /// The connection was re-made: every keyed subscribe has to be sent again.
    pub fn forget_requested(&mut self) {
        self.requested.clear();
    }

    /// The selected index into `board.cards`, defaulting to the first card.
    pub fn selected_index(&self, board: &Board) -> Option<usize> {
        if board.cards.is_empty() {
            return None;
        }
        Some(
            self.selected
                .as_ref()
                .and_then(|l| board.cards.iter().position(|c| &c.label == l))
                .unwrap_or(0),
        )
    }

    fn select_index(&mut self, board: &Board, i: usize) {
        if let Some(c) = board.cards.get(i) {
            self.selected = Some(c.label.clone());
        }
    }

    pub fn set_note(&mut self, s: impl Into<String>, tone: Tone) {
        self.note = Some((s.into(), tone));
    }

    pub fn key(&mut self, k: KeyEvent) -> Option<Action> {
        if k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('c' | 'q')) {
            self.quit = true;
            return None;
        }
        match std::mem::replace(&mut self.prompt, Prompt::None) {
            Prompt::Confirm(a) => {
                return match k.code {
                    KeyCode::Char('y') | KeyCode::Enter => Some(a),
                    _ => {
                        self.set_note("cancelled — nothing was sent", Tone::Muted);
                        None
                    }
                };
            }
            Prompt::Share { label, mut buf } => {
                match k.code {
                    KeyCode::Esc => self.set_note("cancelled — nothing was sent", Tone::Muted),
                    KeyCode::Backspace => {
                        buf.pop();
                        self.prompt = Prompt::Share { label, buf };
                    }
                    KeyCode::Char(c) if c.is_ascii_digit() || c == '.' => {
                        if buf.len() < 6 {
                            buf.push(c);
                        }
                        self.prompt = Prompt::Share { label, buf };
                    }
                    KeyCode::Enter => {
                        if buf.is_empty() {
                            self.prompt = Prompt::Confirm(Action::SetShare(label, None));
                        } else {
                            match buf.parse::<f64>() {
                                Ok(p) if p > 0.0 && p <= 100.0 => {
                                    self.prompt = Prompt::Confirm(Action::SetShare(label, Some(p / 100.0)))
                                }
                                Ok(p) if p == 0.0 => self.set_note(
                                    "a share of 0 is refused — press d to disable the account instead",
                                    Tone::Warn,
                                ),
                                _ => self.set_note("a share is a percentage in (0, 100]", Tone::Warn),
                            }
                        }
                    }
                    _ => self.prompt = Prompt::Share { label, buf },
                }
                return None;
            }
            Prompt::None => {}
        }

        let board = self.board();
        let sel = self.selected_index(&board);
        let card = sel.and_then(|i| board.cards.get(i));
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(i) = sel {
                    self.select_index(&board, (i + 1) % board.cards.len());
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(i) = sel {
                    let n = board.cards.len();
                    self.select_index(&board, (i + n - 1) % n);
                }
            }
            KeyCode::Char(c @ '1'..='9') => {
                let i = c as usize - '1' as usize;
                if i < board.cards.len() {
                    self.select_index(&board, i);
                }
            }
            KeyCode::Char('d' | 'D') => match card {
                Some(c) if c.disabled() => self.prompt = Prompt::Confirm(Action::Enable(c.label.clone())),
                Some(c) => self.prompt = Prompt::Confirm(Action::Disable(c.label.clone())),
                None => self.set_note("no account selected", Tone::Muted),
            },
            KeyCode::Char('s') => match card {
                Some(c) => self.prompt = Prompt::Confirm(Action::SetLive(c.label.clone())),
                None => self.set_note("no account selected", Tone::Muted),
            },
            KeyCode::Char('S') => match card {
                Some(c) => {
                    self.prompt = Prompt::Share { label: c.label.clone(), buf: String::new() }
                }
                None => self.set_note("no account selected", Tone::Muted),
            },
            _ => {}
        }
        None
    }
}
