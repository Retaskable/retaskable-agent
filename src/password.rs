//! The settings page's network-access password (agent-settings.md section 3.1): kept as
//! `scrypt$N$r$p$salt$hash`, hex for the salt and the hash, and compared in constant time.
//! `N`, `r` and `p` are Node's own `crypto.scrypt` defaults (16384, 8, 1) with a 64-byte key, so a
//! hash made by either agent reads the same way.
use rand::RngExt;
use scrypt::Params;

const LOG_N: u8 = 14; // N = 2^14 = 16384.
const R: u32 = 8;
const P: u32 = 1;
const KEY_LEN: usize = 64;
const SALT_LEN: usize = 16;

/// Hashes `password` with a fresh random salt, as `scrypt$16384$8$1$<salt>$<hash>`.
pub fn hash_password(password: &str) -> String {
    let mut salt = [0u8; SALT_LEN];
    rand::rng().fill(&mut salt);
    let hash = derive(password, &salt);
    format!(
        "scrypt${}${}${}${}${}",
        1u32 << LOG_N,
        R,
        P,
        hex_encode(&salt),
        hex_encode(&hash)
    )
}

/// Whether `password` matches a hash `hash_password` made. A hash in a form this agent doesn't
/// recognise (hand-edited, from a future format) never matches rather than erroring.
pub fn verify_password(password: &str, hash: &str) -> bool {
    let Some(parsed) = parse(hash) else {
        return false;
    };
    let Ok(params) = Params::new(parsed.n.trailing_zeros() as u8, parsed.r, parsed.p) else {
        return false;
    };
    let mut actual = vec![0u8; parsed.key.len()];
    if scrypt::scrypt(password.as_bytes(), &parsed.salt, &params, &mut actual).is_err() {
        return false;
    }
    constant_time_eq(&actual, &parsed.key)
}

fn derive(password: &str, salt: &[u8]) -> Vec<u8> {
    let params = Params::new(LOG_N, R, P).expect("fixed scrypt params are valid");
    let mut out = vec![0u8; KEY_LEN];
    scrypt::scrypt(password.as_bytes(), salt, &params, &mut out).expect("fixed scrypt params are valid");
    out
}

struct ParsedHash {
    n: u32,
    r: u32,
    p: u32,
    salt: Vec<u8>,
    key: Vec<u8>,
}

fn parse(hash: &str) -> Option<ParsedHash> {
    let mut parts = hash.split('$');
    if parts.next()? != "scrypt" {
        return None;
    }
    let n: u32 = parts.next()?.parse().ok()?;
    let r: u32 = parts.next()?.parse().ok()?;
    let p: u32 = parts.next()?.parse().ok()?;
    let salt = hex_decode(parts.next()?)?;
    let key = hex_decode(parts.next()?)?;
    if parts.next().is_some() || !n.is_power_of_two() {
        return None;
    }
    Some(ParsedHash { n, r, p, salt, key })
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Compares two byte strings without branching on where they first differ. Different lengths are
/// never equal, but that's public shape (the stored key length), not the secret being compared.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hash_has_the_documented_shape() {
        let hash = hash_password("correct horse battery staple");
        let fields: Vec<&str> = hash.split('$').collect();
        assert_eq!(fields.len(), 6);
        assert_eq!(fields[0], "scrypt");
        assert_eq!(fields[1], "16384");
        assert_eq!(fields[2], "8");
        assert_eq!(fields[3], "1");
        assert_eq!(fields[4].len(), SALT_LEN * 2);
        assert_eq!(fields[5].len(), KEY_LEN * 2);
    }

    #[test]
    fn the_right_password_verifies() {
        let hash = hash_password("correct horse battery staple");
        assert!(verify_password("correct horse battery staple", &hash));
    }

    #[test]
    fn a_wrong_password_does_not_verify() {
        let hash = hash_password("correct horse battery staple");
        assert!(!verify_password("wrong", &hash));
    }

    #[test]
    fn two_hashes_of_the_same_password_differ_by_salt() {
        assert_ne!(hash_password("same"), hash_password("same"));
    }

    #[test]
    fn a_hash_in_an_unrecognised_shape_never_verifies() {
        for bad in [
            "",
            "scrypt$16384$8$1$zz$aa",
            "argon2$...",
            "scrypt$16384$8$1$aa$aa$extra",
        ] {
            assert!(!verify_password("anything", bad), "{bad:?} should not verify");
        }
    }
}
