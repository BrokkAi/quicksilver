use std::process::Command;

/// Build and run the example the way trycmd runs clap's examples: a `cargo`
/// child process that compiles the workspace containing the current
/// directory.
#[test]
fn example_prints_the_greeting() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args(["run", "--quiet", "--example", "greet"])
        .output()
        .expect("run cargo");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hello\n");
}
