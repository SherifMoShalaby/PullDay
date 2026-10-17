//! Updates and version reads that run a fake `gemini` or `agy`. They're in their own test binary, with one test, because they set
//! HOME and SHELL for the whole process: `sh -l` then reads the fake home's .profile, which puts the fakes first.

use pullday_core::models::{ItemStatus, SourceKind, UpdatableItem};
use pullday_core::scanner::{discover_gemini_extensions, Scanner};
use pullday_core::shell::{quote, run};
use pullday_core::tools::{agy_version, discover_antigravity_cli};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

#[test]
fn updates_with_fake_tools() {
    let tmp = std::env::temp_dir().join(format!("pullday-fake-tools-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    let p = |rel: &str| tmp.join(rel).to_string_lossy().to_string();
    let write = |rel: &str, text: &str| {
        fs::create_dir_all(tmp.join(rel).parent().unwrap()).unwrap();
        fs::write(tmp.join(rel), text).unwrap();
    };
    let tool = |name: &str, script: &str| {
        write(&format!("bin/{name}"), script);
        fs::set_permissions(
            tmp.join("bin").join(name),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    };
    write(
        "home/.profile",
        &format!("export PATH={}:\"$PATH\"\n", quote(&p("bin"))),
    );
    std::env::set_var("HOME", p("home"));
    std::env::set_var("SHELL", "/bin/sh");

    // `gemini extensions update` exits 0 even when it installed nothing; the re-check must catch that.
    write(
        "src/gemini-extension.json",
        r#"{"name":"src","version":"2.0.0"}"#,
    );
    write(
        "ext/demo/.gemini-extension-install.json",
        &format!(r#"{{"source":"{}","type":"local"}}"#, p("src")),
    );
    let install = format!(
        "cp {} {}",
        quote(&p("src/gemini-extension.json")),
        quote(&p("ext/demo/gemini-extension.json"))
    );
    tool(
        "gemini",
        &format!("#!/bin/sh\n[ \"$FAKE_GEMINI_INSTALLS\" = 1 ] && {install}\nexit 0\n"),
    );
    let scanner = Scanner::new(p("home"));
    let gemini = |installs: bool| {
        write(
            "ext/demo/gemini-extension.json",
            r#"{"name":"demo","version":"1.0.0"}"#,
        );
        std::env::set_var("FAKE_GEMINI_INSTALLS", if installs { "1" } else { "0" });
        let mut item = discover_gemini_extensions(&p("ext")).remove(0);
        item.status = ItemStatus::Updating;
        scanner.update(&item, None)
    };
    let noop = gemini(false);
    assert!(
        !noop.ok && noop.summary.contains("consent"),
        "a no-op update fails with the consent note: {noop:?}"
    );
    let real = gemini(true);
    assert!(real.ok, "a real update succeeds: {real:?}");

    // A plugin whose origin is a local path: the update refuses and agy is never run.
    tool(
        "agy",
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n  --version) echo 'ERROR: logging before google.Init: I1016 cli.go:27] x' >&2; echo 1.4.0;;\n  plugin) [ \"$2\" = install ] && touch {};;\nesac\nexit 0\n",
            quote(&p("agy-install-ran"))
        ),
    );
    let plugin = p("plugins/local");
    let setup = format!(
        "git init -q {q} && git -C {q} remote add origin /some/local/path",
        q = quote(&plugin)
    );
    assert!(run(&setup, Duration::from_secs(30)).ok());
    let mut local = UpdatableItem::new(SourceKind::AntigravityPlugin, "local", "v1", None);
    local.path = Some(plugin);
    let refused = scanner.update(&local, None);
    assert!(
        !refused.ok && refused.summary.contains("wasn't installed from a git repo"),
        "{refused:?}"
    );
    assert!(
        !tmp.join("agy-install-ran").exists(),
        "agy was run with a local-path origin"
    );
    // agy's version is read past its log line, and the CLI item shows it.
    assert_eq!(agy_version().as_deref(), Some("1.4.0"));
    let cli = discover_antigravity_cli();
    assert_eq!(
        (cli.len(), cli[0].id.as_str(), cli[0].detail.as_str()),
        (1, "antigravityCLI:agy", "v1.4.0")
    );
    assert!(
        cli[0]
            .path
            .as_deref()
            .is_some_and(|p| p.ends_with("/bin/agy")),
        "{:?}",
        cli[0].path
    );
    // The fake is really what runs, so the check above means something.
    assert!(
        run("agy plugin install x", Duration::from_secs(30)).ok()
            && tmp.join("agy-install-ran").exists()
    );

    fs::remove_dir_all(&tmp).unwrap();
}
