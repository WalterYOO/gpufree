//! JSON rendering. The schema (`gpufree/v1`) lives in `model`, so this is
//! only serialization; key order follows the struct declarations.

use crate::model::Snapshot;

pub fn render(snapshot: &Snapshot) -> serde_json::Result<String> {
    let mut text = serde_json::to_string_pretty(snapshot)?;
    text.push('\n');
    Ok(text)
}
