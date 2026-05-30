use super::helpers;
use predicates::prelude::*;

#[test]
fn completions_bash() {
    helpers::cmd()
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("engramdb"));
}

#[test]
fn completions_bash_has_shell_content() {
    let output = helpers::cmd()
        .args(["completions", "bash"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Bash completions should contain shell-specific patterns
    assert!(
        stdout.contains("complete") || stdout.contains("COMPREPLY") || stdout.contains("_engramdb"),
        "bash completions should contain bash-specific patterns: {}",
        &stdout[..200.min(stdout.len())]
    );
}

#[test]
fn completions_zsh() {
    helpers::cmd()
        .args(["completions", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("engramdb"));
}

#[test]
fn completions_fish() {
    helpers::cmd()
        .args(["completions", "fish"])
        .assert()
        .success()
        .stdout(predicate::str::contains("engramdb"));
}

#[test]
fn completions_fish_has_shell_content() {
    let output = helpers::cmd()
        .args(["completions", "fish"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Fish completions should contain fish-specific patterns
    assert!(
        stdout.contains("complete") || stdout.contains("-c engramdb"),
        "fish completions should contain fish-specific patterns: {}",
        &stdout[..200.min(stdout.len())]
    );
}
