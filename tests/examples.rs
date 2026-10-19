//! `examples/` is what a stranger copies first, so it must start the server: the env file and the
//! identities file load through the real configuration, and the tokens it documents match its hashes.

use sha2::{Digest, Sha256};

use seerr_mcp::config::Config;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

fn example_env() -> Vec<(String, String)> {
    std::fs::read_to_string(format!("{ROOT}/examples/seerr-mcp.env"))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            let (k, v) = l.split_once('=').expect("KEY=value");
            (k.to_string(), v.to_string())
        })
        .collect()
}

#[test]
fn the_example_household_loads() {
    let env = example_env();
    let identities = format!("{ROOT}/examples/identities.yml");
    let config = Config::from_lookup(|key| {
        if key == "SEERR_MCP_IDENTITIES" {
            return Some(identities.clone());
        }
        env.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    })
    .unwrap();

    let profiles: Vec<&str> = config
        .identities
        .iter()
        .map(|i| i.profile.as_str())
        .collect();
    assert_eq!(profiles, ["alice", "bob"]);
    for who in &config.identities {
        let token = format!("example-token-{}", who.profile);
        let hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        assert_eq!(who.token_sha256, hash, "{}'s documented token", who.profile);
    }
}
