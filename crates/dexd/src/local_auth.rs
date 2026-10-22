//! #2288: the local endpoint's handshake — the token never crosses the socket.
//!
//! The desktop shell found `dexd --local` through a discovery file any same-user process can
//! rewrite, and sent the raw token in its first frame to whatever that file named. Now the shell
//! trusts only the numeric `port` and its own token file, and proves the token instead of
//! sending it:
//!
//! 1. client → `auth.hello {protocol, probe?}` (no token);
//! 2. server → `{challenge, proof}`: a random 32-byte challenge and `HMAC(key, "server" ‖
//!    challenge ‖ port)` — the client checks it before saying anything else, so a listener that
//!    does not hold the token learns nothing and gets nothing;
//! 3. client → `auth.proof {proof}` = `HMAC(key, "client" ‖ challenge ‖ port)`;
//! 4. server verifies in constant time → the usual hello result (`device.id = "local"`).
//!
//! `key` = SHA-256 of the token: what the server already keeps (`LocalToken`), what the client
//! derives from its file copy. `port` = the TCP port the client dialed / the server accepted on:
//! a listener on another port cannot relay the exchange to the real core (its proofs carry the
//! wrong port). HMAC-SHA256 is spelled out here (RFC 2104 over `sha2`) so the shell can include
//! this file without linking the crate (`#[path]`, like `private_fs.rs`).

use sha2::{Digest, Sha256};

const SERVER_LABEL: &[u8] = b"dex-local-auth/1/server";
const CLIENT_LABEL: &[u8] = b"dex-local-auth/1/client";

/// The HMAC key both sides hold: the token's digest.
pub fn key(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// What the server sends with its challenge: proof that it holds the token, bound to the port.
pub fn server_proof(key: &[u8; 32], challenge: &[u8; 32], port: u16) -> [u8; 32] {
    hmac_sha256(key, &[SERVER_LABEL, challenge, &port.to_be_bytes()])
}

/// What the client answers with: proof that it holds the token, bound to the same port.
pub fn client_proof(key: &[u8; 32], challenge: &[u8; 32], port: u16) -> [u8; 32] {
    hmac_sha256(key, &[CLIENT_LABEL, challenge, &port.to_be_bytes()])
}

/// Constant time: the first mismatching byte says nothing about where it is.
pub fn ct_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// RFC 2104 HMAC over SHA-256 (block 64 bytes); a key over one block is hashed first.
pub fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let hashed: [u8; 32];
    let key: &[u8] = if key.len() > 64 {
        hashed = Sha256::digest(key).into();
        &hashed
    } else {
        key
    };
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for (i, k) in key.iter().enumerate() {
        ipad[i] ^= k;
        opad[i] ^= k;
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    for p in parts {
        inner.update(p);
    }
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    outer.finalize().into()
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Exactly 64 hex digits → 32 bytes; anything else is None (a malformed peer, not a panic).
pub fn unhex32(s: &str) -> Option<[u8; 32]> {
    let s = s.as_bytes();
    if s.len() != 64 {
        return None;
    }
    let nibble = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut out = [0u8; 32];
    for (i, pair) in s.chunks(2).enumerate() {
        out[i] = nibble(pair[0])? << 4 | nibble(pair[1])?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4231 test case 2 (key "Jefe") and 3 (20 × 0xaa; 50 × 0xdd): the construction is the
    /// standard one, with the message given in parts.
    #[test]
    fn hmac_sha256_matches_rfc_4231() {
        let m = hmac_sha256(b"Jefe", &[b"what do ya want ", b"for nothing?"]);
        assert_eq!(hex(&m), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
        let m = hmac_sha256(&[0xaa; 20], &[&[0xdd; 50]]);
        assert_eq!(hex(&m), "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe");
        // Test case 6: a key longer than the block is hashed first.
        let m = hmac_sha256(&[0xaa; 131], &[b"Test Using Larger Than Block-Size Key - Hash Key First"]);
        assert_eq!(hex(&m), "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54");
    }

    /// Role, challenge, port and key each change the proof; the two sides agree on the same key.
    #[test]
    fn proofs_are_bound_to_role_challenge_port_and_token() {
        let k = key("a".repeat(64).as_str());
        let c = [7u8; 32];
        let s = server_proof(&k, &c, 4242);
        let cl = client_proof(&k, &c, 4242);
        assert!(ct_eq(&s, &server_proof(&k, &c, 4242)) && ct_eq(&cl, &client_proof(&k, &c, 4242)));
        assert!(!ct_eq(&s, &cl), "a server proof is no client proof");
        assert!(!ct_eq(&s, &server_proof(&k, &c, 4243)), "another port: a relay");
        assert!(!ct_eq(&s, &server_proof(&k, &[8u8; 32], 4242)), "another challenge");
        assert!(!ct_eq(&s, &server_proof(&key("b"), &c, 4242)), "another token");
        let digest: [u8; 32] = Sha256::digest(b"a".repeat(64)).into();
        assert_eq!(k, digest, "the key is the token's digest");
    }

    #[test]
    fn hex_round_trips_and_rejects_the_malformed() {
        let b: [u8; 32] = std::array::from_fn(|i| (i * 7) as u8);
        assert_eq!(unhex32(&hex(&b)), Some(b));
        assert_eq!(unhex32(&hex(&b).to_uppercase()), Some(b));
        assert_eq!(unhex32(&hex(&b)[..63]), None);
        assert_eq!(unhex32(&format!("{}zz", &hex(&b)[..62])), None);
        assert_eq!(unhex32(""), None);
    }
}
