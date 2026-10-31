//! The model choice (Sonnet / Opus). Mirrors `aui_nav::scope::Model` by variant
//! so the server can map between them without coupling this crate to aui-nav.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Model {
    #[default]
    Sonnet,
    Opus,
    Fable,
}

impl Model {
    /// Display/spoken label.
    pub fn label(self) -> &'static str {
        match self {
            Model::Sonnet => "Sonnet",
            Model::Opus => "Opus",
            Model::Fable => "Fable",
        }
    }

    /// Cycle used by the options menus: None -> Sonnet -> Opus -> Fable -> None.
    pub fn cycle_opt(current: Option<Model>) -> Option<Model> {
        match current {
            None => Some(Model::Sonnet),
            Some(Model::Sonnet) => Some(Model::Opus),
            Some(Model::Opus) => Some(Model::Fable),
            Some(Model::Fable) => None,
        }
    }
}

impl std::fmt::Display for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Render an optional model override into a spoken/displayed string.
pub fn model_label(current: Option<Model>) -> String {
    match current {
        Some(m) => format!("model: {m}"),
        None => "model: default".into(),
    }
}
