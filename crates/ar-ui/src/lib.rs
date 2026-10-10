//! Native text-first accessibility primitives. No model, capture, transport or global input.
//! Caller owns authorization and verification; keep native handles on the creating thread.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(windows, target_os = "macos")))]
mod unsupported;
#[cfg(windows)]
mod windows;
#[cfg(target_os = "macos")]
pub use macos::*;
#[cfg(not(any(windows, target_os = "macos")))]
pub use unsupported::*;
#[cfg(windows)]
pub use windows::*;
#[derive(Debug, Clone, Serialize)]
pub struct Node {
    pub id: String,
    pub parent: Option<String>,
    pub depth: usize,
    pub role: String,
    pub label: String,
    pub identifier: String,
    pub value: Option<String>,
    pub enabled: Option<bool>,
    pub focused: Option<bool>,
    pub visible: Option<bool>,
    pub expanded: Option<bool>,
    pub read_only: Option<bool>,
    pub selected: Option<bool>,
    pub checked: Option<bool>,
    pub protected: bool,
    pub bounds: Option<[f64; 4]>,
    pub actions: Vec<String>,
}
impl Node {
    pub fn empty(role: String) -> Self {
        Self {
            id: String::new(),
            parent: None,
            depth: 0,
            role,
            label: String::new(),
            identifier: String::new(),
            value: None,
            enabled: None,
            focused: None,
            visible: None,
            expanded: None,
            read_only: None,
            selected: None,
            checked: None,
            protected: false,
            bounds: None,
            actions: vec![],
        }
    }
    pub fn same_target(&self, other: &Self) -> bool {
        self.role == other.role
            && self.label == other.label
            && self.identifier == other.identifier
            && self.protected == other.protected
    }
}

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Press,
    SetValue,
    Select,
    Toggle,
    Expand,
    Collapse,
    Focus,
}
impl Action {
    pub fn name(self) -> &'static str {
        match self {
            Self::Press => "press",
            Self::SetValue => "set_value",
            Self::Select => "select",
            Self::Toggle => "toggle",
            Self::Expand => "expand",
            Self::Collapse => "collapse",
            Self::Focus => "focus",
        }
    }
}

/// A bounded native observation; partial results remain explicitly marked.
pub type Tree = (Vec<(Node, Element)>, bool, Vec<String>);
