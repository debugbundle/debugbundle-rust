use std::{env, process::Command};
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let version = env::var_os("RUSTC")
        .and_then(|compiler| Command::new(compiler).arg("--version").output().ok())
        .filter(|output| output.status.success() && output.stdout.len() < 256)
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|line| {
            line.split_whitespace()
                .nth(1)
                .filter(|version| {
                    version.len() <= 64
                        && version
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b".-_".contains(&byte))
                })
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=DEBUGBUNDLE_RUSTC_VERSION={version}");
}
