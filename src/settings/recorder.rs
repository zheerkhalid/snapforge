//! Settings for Phase 4 screen recorder and video editor. Owned by that module's agent: add fields here
//! (keep `#[serde(default)]` so older settings files still load).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RecorderSettings {}
