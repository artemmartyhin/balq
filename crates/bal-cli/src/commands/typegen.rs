use crate::util::load_layout;
use anyhow::Result;
use std::path::Path;

pub fn run(layout: &Path, name: Option<String>) -> Result<()> {
    let l = load_layout(layout)?;
    let name = name.unwrap_or_else(|| {
        let stem = layout
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Contract");
        let stem = stem.split('.').next().unwrap_or(stem);
        let mut c = stem.chars();
        let cap: String = c
            .next()
            .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
            .unwrap_or_default();
        format!("{cap}View")
    });
    print!("{}", l.typescript(&name));
    Ok(())
}
