//! A check the way the app runs one: settings from $XDG_CONFIG_HOME/pullday/config.json, items from $HOME,
//! the log in $XDG_STATE_HOME/pullday/pullday.log.
//!   cargo run --example scan              check, then what the schedule would do (auto-update only if it's on)
//!   cargo run --example scan -- <id>…     then update those items
use pullday_core::prefs::{default_path, Prefs};
use pullday_core::scanner::Scanner;
use pullday_core::shell::{home, log_path};
use pullday_core::state::{describe, Core};

fn main() -> std::io::Result<()> {
    let prefs = Prefs::load(&default_path());
    let mut core = Core::new(Scanner::new(home().to_string_lossy()), prefs, log_path());
    core.refresh(true)?;
    for item in &core.items {
        println!("{}  [{}]  {}", item.id, item.detail, describe(&item.status));
    }
    for notice in core.after_scheduled_check()? {
        println!("notice: {} · {}", notice.title, notice.body);
    }
    let ids: Vec<String> = std::env::args().skip(1).collect();
    for notice in core.update(&ids, true)? {
        println!("notice: {} · {}", notice.title, notice.body);
    }
    Ok(())
}
