//! Settings for Phase 5 screen tools: color picker, magnifier, crosshair, focus, ruler. Owned by that module's agent: add fields here
//! (keep `#[serde(default)]` so older settings files still load).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ToolsSettings {}
