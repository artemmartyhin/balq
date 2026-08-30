//! Terminal presentation: colours only when stdout is a terminal (and
//! `NO_COLOR` is unset), a banner, progress bars. Nothing here decides
//! anything; `--json` bypasses all of it.

use indicatif::{ProgressBar, ProgressStyle};
use owo_colors::{OwoColorize, Stream::Stdout};
use std::fmt::Display;
use std::time::Duration;

pub fn cyan(s: impl Display) -> String {
    format!("{}", s.if_supports_color(Stdout, |t| t.cyan()))
}
pub fn green(s: impl Display) -> String {
    format!("{}", s.if_supports_color(Stdout, |t| t.green()))
}
pub fn yellow(s: impl Display) -> String {
    format!("{}", s.if_supports_color(Stdout, |t| t.yellow()))
}
pub fn red(s: impl Display) -> String {
    format!("{}", s.if_supports_color(Stdout, |t| t.red()))
}
pub fn dim(s: impl Display) -> String {
    format!("{}", s.if_supports_color(Stdout, |t| t.dimmed()))
}
pub fn bold(s: impl Display) -> String {
    format!("{}", s.if_supports_color(Stdout, |t| t.bold()))
}

/// `0x3582…53ce`
pub fn short_addr(a: impl Display) -> String {
    let s = a.to_string();
    if s.len() > 12 {
        format!("{}…{}", &s[..6], &s[s.len() - 4..])
    } else {
        s
    }
}

/// Block numbers and counts as plain digits: they get copied into the next
/// command, so no separators.
pub fn num(n: impl Into<u128>) -> String {
    n.into().to_string()
}

pub fn banner() {
    let logo = [
        "██████╗  █████╗ ██╗      ██████╗ ",
        "██╔══██╗██╔══██╗██║     ██╔═══██╗",
        "██████╔╝███████║██║     ██║   ██║",
        "██╔══██╗██╔══██║██║     ██║▄▄ ██║",
        "██████╔╝██║  ██║███████╗╚██████╔╝",
        "╚═════╝ ╚═╝  ╚═╝╚══════╝ ╚══▀▀═╝ ",
    ];
    println!();
    for (i, line) in logo.iter().enumerate() {
        let tail = match i {
            2 => format!(
                "   {} {}",
                bold(format!("balq {}", env!("CARGO_PKG_VERSION"))),
                dim("· verified storage history from Block Access Lists")
            ),
            3 => format!("   {}", dim("github.com/artemmartyhin/balq")),
            _ => String::new(),
        };
        println!("  {}{}", cyan(line), tail);
    }
    println!();
}

/// `  label     value`
pub fn kv(label: &str, value: impl Display) {
    println!("  {:<9} {}", dim(label), value);
}

pub fn ok(msg: impl Display) {
    println!("  {} {}", green("✓"), msg);
}
pub fn warn(msg: impl Display) {
    println!("  {} {}", yellow("!"), msg);
}
pub fn fail(msg: impl Display) {
    println!("  {} {}", red("✗"), msg);
}

/// Progress over blocks: a bar when their number is known, a counter when it
/// is not (walking back to the deploy).
pub fn walk_bar(prefix: &str, total: Option<u64>) -> ProgressBar {
    let (pb, template) = match total {
        Some(t) => (
            ProgressBar::new(t),
            "  {prefix:<9} {bar:24.cyan/black} {pos}/{len} blocks · at {msg} · {elapsed}",
        ),
        None => (
            ProgressBar::new_spinner(),
            "  {prefix:<9} {spinner:.cyan} {pos} blocks read · at block {msg} · {elapsed}",
        ),
    };
    let style = ProgressStyle::with_template(template)
        .unwrap_or_else(|_| ProgressStyle::default_bar())
        .progress_chars("█▓░");
    pb.set_style(style);
    pb.set_prefix(prefix.to_string());
    pb.enable_steady_tick(Duration::from_millis(120));
    pb
}
