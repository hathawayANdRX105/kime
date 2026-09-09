use std::process::Command;

#[test]
fn status_cli_returns_mode_and_scheme() {
    let output = Command::new(env!("CARGO_BIN_EXE_kime"))
        .arg("status")
        .output()
        .expect("Failed to execute kime status");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("mode:"),
        "status output should contain mode: {}",
        stdout
    );
    assert!(
        stdout.contains("scheme:"),
        "status output should contain scheme: {}",
        stdout
    );
}
