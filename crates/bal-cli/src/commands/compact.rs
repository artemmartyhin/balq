use super::Ctx;
use crate::ui;
use crate::util::emit;
use anyhow::Result;
use bal_archive::Archive;
use serde_json::json;

/// Rewrite the archive file without free pages. Needs the file to itself.
pub fn run(ctx: &Ctx) -> Result<()> {
    let before = std::fs::metadata(&ctx.data).map(|m| m.len()).unwrap_or(0);
    let changed = Archive::compact_file(&ctx.data)?;
    let after = std::fs::metadata(&ctx.data)
        .map(|m| m.len())
        .unwrap_or(before);
    if ctx.json {
        emit(&json!({ "changed": changed, "bytesBefore": before, "bytesAfter": after }));
        return Ok(());
    }
    if changed {
        ui::ok(format!(
            "{} → {} MB",
            format_args!("{:.1}", before as f64 / 1e6),
            format_args!("{:.1}", after as f64 / 1e6)
        ));
    } else {
        ui::ok(format!("already compact ({:.1} MB)", before as f64 / 1e6));
    }
    Ok(())
}
