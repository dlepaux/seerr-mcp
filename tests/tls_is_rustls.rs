//! The image builds no libssl, native or cross (Dockerfile), because the dependency tree holds none.
//! A crate pulling `openssl-sys` (native-tls, via a default feature) must fail here, in `cargo test`,
//! not later in the arm64 cross-build, which cannot pair a foreign libssl-dev with the host's libssl3
//! whenever Debian publishes one architecture before the other (2026-09-25).

#[test]
fn the_lock_holds_no_openssl() {
    let lock = include_str!("../Cargo.lock");
    for name in ["openssl-sys", "native-tls", "openssl"] {
        assert!(
            !lock.lines().any(|l| l == format!("name = \"{name}\"")),
            "Cargo.lock pulls `{name}`: use rustls, or provision libssl in the Dockerfile for both legs"
        );
    }
}
