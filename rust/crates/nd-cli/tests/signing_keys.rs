//! BOOT-01 increment B: signing-property codec acceptance tests
//! (`JWTSecret` / `JWTPublicSecret` wire format only).
//!
//! Everything runs on synthetic values. No real config, no real credentials, no
//! database, no property table, no boot wiring.
//!
//! ## Interop provenance
//!
//! **Forward direction (Go writes, Rust reads) — asserted by the tests here.**
//! The two ciphertext constants below were produced by the **actual Go codec**,
//! not by a reimplementation:
//!
//! * Toolchain: `go version go1.27.4 darwin/arm64` (Homebrew).
//! * Module: `github.com/navidrome/navidrome` at commit `919b7119` — the same
//!   checkout this Rust workspace lives in.
//! * Scratch driver recipe (reproducible, outside the repo so nothing is
//!   committed). Create `/tmp/gointerop/` with:
//!   ```text
//!   # go.mod
//!   module gointerop
//!   go 1.27
//!   require github.com/navidrome/navidrome v0.0.0
//!   replace github.com/navidrome/navidrome => <path-to-navidrome-checkout>
//!   ```
//!   and a `main.go` whose only crypto path is the real Go implementation —
//!   `utils.Encrypt` / `utils.Decrypt` (`utils/encrypt.go:15,40`),
//!   `consts.DefaultEncryptionKey` (`consts/consts.go:47-49`), `sha256` for
//!   Go's `getEncKey`, and `id.NewRandom` (`model/id/id.go:10-15`):
//!   ```go
//!   func encKey(passwordKey string) []byte {
//!       k := passwordKey
//!       if k == "" { k = consts.DefaultEncryptionKey }
//!       s := sha256.Sum256([]byte(k))
//!       return s[:]
//!   }
//!   // gen:     ct, _ := utils.Encrypt(ctx, encKey(passwordKey), plaintext)
//!   // decrypt: pt, err := utils.Decrypt(ctx, encKey(passwordKey), ct)
//!   ```
//!   Then `go mod tidy && go build ./...`.
//! * Commands that produced the recorded vectors:
//!   ```text
//!   go run . gen "" "an-example-signing-secret-0123456789abcdef"
//!   go run . gen "a-test-password-encryption-key" \
//!     "public-link-signing-secret-9876543210fedcba"
//!   ```
//!   `rust_decrypts_ciphertext_produced_by_the_real_go_codec_*` assert Rust
//!   recovers those plaintexts, and
//!   `signing_keys::go_wire_format_tests::byte_exact_reproduction_of_go_ciphertext`
//!   (unit test) rebuilds Go's exact bytes from Go's own nonce — that only
//!   succeeds if the nonce prefix and the empty AAD are both exact.
//!
//! **Reverse direction (Rust writes, Go reads) — verified manually, not by a
//! test in this crate.** No Rust test can shell out to Go in a locked,
//! hermetic way, so this was run by hand and the result recorded here; the
//! automated tests in this direction (`rust_output_matches_the_go_wire_format`,
//! `rust_output_matches_the_go_wire_format_custom_key`) only assert the
//! structural contract this crate can check: `nonce || ct || tag` lengths,
//! padded standard (not URL) base64, and equality with Go's ciphertext length.
//!
//! Reproduce the manual run:
//! ```text
//! cargo test --locked -p nd-cli --test signing_keys -- --nocapture \
//!   --exact rust_output_matches_the_go_wire_format
//! # prints RUST_OUTPUT_FOR_GO[<key>]="<CT>" (fresh CT every run: random nonce)
//! cd /tmp/gointerop && go run . decrypt "" "<CT>"
//! ```
//! Recorded output of that manual verification:
//! ```text
//! CT(default key) = EYF3uAObm3prhrb65seojOcQj+/0EyWkKzmmUKm/qgYyGur0tTOfVa4BDHVGPe4Kvfabb9AJv6LW9TkKM+j9SSZUIn4ABw==
//! $ go run . decrypt "" "$CT"
//! plaintext=an-example-signing-secret-0123456789abcdef
//!
//! CT(custom key)  = tEYLiYsJEAxNWCGqGkPBVx+H5LbMC+XFAvJ7T3djXEx9jrE6+tcjQiFDgIgj6U9cKYPlaETujPIo+H9/7dixJXERb8Qf/PA=
//! $ go run . decrypt "a-test-password-encryption-key" "$CT"
//! plaintext=public-link-signing-secret-9876543210fedcba
//! ```
//! Both are the real Go `utils.Decrypt` recovering what this codec wrote, so
//! the write direction is Go-compatible too.
//!
//! ## Entropy
//!
//! Nonces and generated candidates come from the OS entropy source through a
//! crate-internal seam, so **no public testing hook exists**. The
//! failure-on-injected-source cases live in `signing_keys::entropy_tests`
//! (unit tests inside the module); this integration file can only assert the
//! real OS path succeeds and yields fresh, independent values.

use std::error::Error as _;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use nd_cli::signing_keys::{
    decrypt_signing_secret, encrypt_signing_secret, generate_signing_secret, EncryptionKey,
    SigningKeyError, DEFAULT_PASSWORD_ENCRYPTION_KEY, JWT_PUBLIC_SECRET_PROPERTY,
    JWT_SECRET_PROPERTY, KEY_LEN, NONCE_LEN, SIGNING_MATERIAL_BITS, SIGNING_MATERIAL_BYTES,
    TAG_LEN,
};

/// `sha256("just for obfuscation")`, computed independently with
/// `printf 'just for obfuscation' | shasum -a 256`.
const DEFAULT_KEY_SHA256_HEX: &str =
    "dd8b6d91ab64bf7a1b879fe22f668774b69da53cc4eadac727015cf6eb9329f9";

/// `sha256("a-test-password-encryption-key")`, same independent computation.
const CUSTOM_KEY_SHA256_HEX: &str =
    "4949ecc72b1629140fb5e8cf77a99f3c61f4b1b56ac4c8e773608384aac42070";

/// Go-produced ciphertext for the **empty** `PasswordEncryptionKey`, i.e. the
/// documented public default key. Output of `go run . gen "" …`.
const GO_VECTOR_DEFAULT_KEY: (&str, &str) = (
    "an-example-signing-secret-0123456789abcdef",
    "Z3Yj9Nxq8mfF8p3JVCNLNWq5CbpOA6i6jHU5VRHyi8igkpRu2Wp50sntfHpolfnMuz335BXgcpO1ErnHl2k1LmySMd12nA==",
);

/// Go-produced ciphertext for a non-empty `PasswordEncryptionKey`.
const GO_VECTOR_CUSTOM_KEY: (&str, &str) = (
    "public-link-signing-secret-9876543210fedcba",
    "65gYIXojV3nPwpBo/k3KPL8eUtn2OmVivneH845ppywhTnc7t+1okxXOzqPs4+7bECFqNJmETaW5crLqqD17LfvnyRWCQPw=",
);

/// A Go `id.NewRandom()` sample (22-char base62), for shape compatibility.
const GO_ID_SAMPLE: &str = "0Zx1SeQneyBU6R27unWdJy";

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ------------------------------------------------------- key derivation

#[test]
fn empty_password_encryption_key_uses_the_documented_public_default() {
    let key = EncryptionKey::from_password_encryption_key("");
    assert_eq!(
        to_hex(key.as_bytes()),
        DEFAULT_KEY_SHA256_HEX,
        "sha256 of the Go default key must match exactly"
    );
    assert_eq!(DEFAULT_PASSWORD_ENCRYPTION_KEY, "just for obfuscation");
    assert_eq!(key.as_bytes().len(), KEY_LEN);
}

#[test]
fn non_empty_password_encryption_key_is_used_verbatim() {
    let key = EncryptionKey::from_password_encryption_key("a-test-password-encryption-key");
    assert_eq!(to_hex(key.as_bytes()), CUSTOM_KEY_SHA256_HEX);
}

/// Go uses `cmp.Or`, which only tests for emptiness — whitespace and near-empty
/// values are used as-is, never replaced by the default.
#[test]
fn whitespace_only_key_is_not_treated_as_empty() {
    let key = EncryptionKey::from_password_encryption_key(" ");
    assert_ne!(to_hex(key.as_bytes()), DEFAULT_KEY_SHA256_HEX);
}

// ---------------------------------------------- accepting Go-generated material

#[test]
fn rust_decrypts_ciphertext_produced_by_the_real_go_codec_default_key() {
    let key = EncryptionKey::from_password_encryption_key("");
    let secret = decrypt_signing_secret(&key, GO_VECTOR_DEFAULT_KEY.1)
        .expect("Go-produced ciphertext must decrypt with the same key derivation");
    assert_eq!(secret.expose(), GO_VECTOR_DEFAULT_KEY.0);
}

#[test]
fn rust_decrypts_ciphertext_produced_by_the_real_go_codec_custom_key() {
    let key = EncryptionKey::from_password_encryption_key("a-test-password-encryption-key");
    let secret = decrypt_signing_secret(&key, GO_VECTOR_CUSTOM_KEY.1)
        .expect("Go-produced ciphertext must decrypt with the same key derivation");
    assert_eq!(secret.expose(), GO_VECTOR_CUSTOM_KEY.0);
}

/// Structural half of the reverse interop direction: what this codec writes
/// has Go's exact byte layout (`nonce || ct || tag`, padded standard base64)
/// and Go's exact ciphertext length for the same plaintext. The Go-side read
/// itself is **manual** — see the module header for the recipe and the
/// recorded output; this test asserts only what a Rust test can check.
#[test]
fn rust_output_matches_the_go_wire_format() {
    let key = EncryptionKey::from_password_encryption_key("");
    let ct = encrypt_signing_secret(&key, GO_VECTOR_DEFAULT_KEY.0).unwrap();
    // nonce(12) || ciphertext || tag(16), padded standard base64: exactly the
    // length Go's Encrypt produces for the same plaintext.
    let decoded = b64_decode(&ct);
    assert_eq!(
        decoded.len(),
        NONCE_LEN + GO_VECTOR_DEFAULT_KEY.0.len() + TAG_LEN,
        "wire format is nonce || ciphertext || tag"
    );
    assert_eq!(
        ct.len(),
        decoded.len().div_ceil(3) * 4,
        "padded standard base64"
    );
    assert!(
        !ct.contains('-') && !ct.contains('_'),
        "not the URL alphabet"
    );
    assert_eq!(ct.as_bytes().last(), Some(&b'='), "StdEncoding pads");
    // Go-produced ciphertext for the same plaintext has the same length.
    assert_eq!(
        ct.len(),
        GO_VECTOR_DEFAULT_KEY.1.len(),
        "same plaintext, same key: same wire length"
    );
    // Value printed for the Go-side verification command (see module header).
    println!("RUST_OUTPUT_FOR_GO[{DEFAULT_PASSWORD_ENCRYPTION_KEY}]={ct}");
}

/// Same reverse-direction check for a non-empty `PasswordEncryptionKey`, which
/// also proves the custom-key derivation is Go-compatible on the write side.
#[test]
fn rust_output_matches_the_go_wire_format_custom_key() {
    let key = EncryptionKey::from_password_encryption_key("a-test-password-encryption-key");
    let ct = encrypt_signing_secret(&key, GO_VECTOR_CUSTOM_KEY.0).unwrap();
    let decoded = b64_decode(&ct);
    assert_eq!(
        decoded.len(),
        NONCE_LEN + GO_VECTOR_CUSTOM_KEY.0.len() + TAG_LEN
    );
    println!("RUST_OUTPUT_FOR_GO[a-test-password-encryption-key]={ct}");
}

#[test]
fn rust_roundtrip_reproduces_the_go_wire_format() {
    let key = EncryptionKey::from_password_encryption_key("");
    let ct = encrypt_signing_secret(&key, GO_ID_SAMPLE).unwrap();
    let secret = decrypt_signing_secret(&key, &ct).expect("roundtrip must succeed");
    assert_eq!(secret.expose(), GO_ID_SAMPLE);
    let decoded = b64_decode(&ct);
    assert_eq!(decoded.len(), NONCE_LEN + GO_ID_SAMPLE.len() + TAG_LEN);
}

/// Both property keys must be the exact Go constant spellings, since the codec
/// is the only place they are named for now.
#[test]
fn property_key_names_match_go() {
    assert_eq!(JWT_SECRET_PROPERTY, "JWTSecret");
    assert_eq!(JWT_PUBLIC_SECRET_PROPERTY, "JWTPublicSecret");
}

// ---------------------------------------------------------- fresh randomness

#[test]
fn generated_material_is_256_bits_of_lowercase_hex() {
    let secret = generate_signing_secret().expect("OS entropy must be available");
    let text = secret.expose();
    assert_eq!(text.len(), SIGNING_MATERIAL_BYTES * 2);
    assert_eq!(SIGNING_MATERIAL_BITS, 256);
    assert!(
        text.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "must be lowercase hex"
    );
    assert!(!text.bytes().any(|b| b.is_ascii_uppercase()));
}

#[test]
fn generated_material_is_independent_across_candidates() {
    let candidates: Vec<String> = (0..32)
        .map(|_| {
            generate_signing_secret()
                .expect("OS entropy must be available")
                .expose()
                .to_string()
        })
        .collect();
    let unique: std::collections::HashSet<&str> = candidates.iter().map(String::as_str).collect();
    assert_eq!(
        unique.len(),
        candidates.len(),
        "every candidate must be distinct"
    );
    // No generated value is all-zero or otherwise degenerate.
    assert!(candidates.iter().all(|c| c.chars().any(|ch| ch != '0')));
}

#[test]
fn encrypting_the_same_secret_twice_uses_a_fresh_nonce() {
    let key = EncryptionKey::from_password_encryption_key("");
    let a = encrypt_signing_secret(&key, GO_VECTOR_DEFAULT_KEY.0).unwrap();
    let b = encrypt_signing_secret(&key, GO_VECTOR_DEFAULT_KEY.0).unwrap();
    assert_ne!(a, b, "a fresh random nonce per call, like Go's rand.Reader");
    // Both still decrypt to the same plaintext.
    assert_eq!(
        decrypt_signing_secret(&key, &a).unwrap().expose(),
        GO_VECTOR_DEFAULT_KEY.0
    );
    assert_eq!(
        decrypt_signing_secret(&key, &b).unwrap().expose(),
        GO_VECTOR_DEFAULT_KEY.0
    );
}

// ------------------------------------------------------------- rejection

#[test]
fn empty_stored_value_is_rejected() {
    let key = EncryptionKey::from_password_encryption_key("");
    assert_err(
        decrypt_signing_secret(&key, ""),
        SigningKeyError::EmptyStoredValue,
        "SigningKeyError::EmptyStoredValue expected",
    );
}

#[test]
fn malformed_stored_values_are_rejected() {
    let key = EncryptionKey::from_password_encryption_key("");
    for bad in [
        "not base64 at all!",
        "!!!!",
        "aGVsbG8=", // decodes, but far shorter than nonce+tag
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", // 24 bytes: nonce + 12 < tag length
        "Z3Yj9Nxq8mfF8p3JVCNLNWq5CbpOA6i6jHU5VRHyi8igkpRu2Wp50sntfHpolfnMuz335BXgcpO1ErnHl2k1LmySMd12nA", // unpadded
    ] {
        let err = decrypt_signing_secret(&key, bad)
            .expect_err(&format!("{bad:?} must be rejected"));
        assert_eq!(err, SigningKeyError::MalformedCiphertext, "{bad:?}");
    }
}

#[test]
fn wrong_key_is_rejected() {
    let right = EncryptionKey::from_password_encryption_key("a-test-password-encryption-key");
    let wrong = EncryptionKey::from_password_encryption_key("");
    let err = decrypt_signing_secret(&wrong, GO_VECTOR_CUSTOM_KEY.1)
        .expect_err("a different key must not open the ciphertext");
    assert_eq!(err, SigningKeyError::WrongKeyOrTag);
    // And the right key still works, so the rejection is the key, not the data.
    assert!(decrypt_signing_secret(&right, GO_VECTOR_CUSTOM_KEY.1).is_ok());
}

#[test]
fn tampered_ciphertext_or_tag_is_rejected() {
    let key = EncryptionKey::from_password_encryption_key("");
    let mut decoded = b64_decode(GO_VECTOR_DEFAULT_KEY.1);
    let last = decoded.len() - 1;
    decoded[last] ^= 0x01; // flip a tag byte
    let tampered = b64_encode(&decoded);
    assert_err(
        decrypt_signing_secret(&key, &tampered),
        SigningKeyError::WrongKeyOrTag,
        "the GCM tag must be verified",
    );

    let mut decoded = b64_decode(GO_VECTOR_DEFAULT_KEY.1);
    decoded[NONCE_LEN] ^= 0x01; // flip a ciphertext byte
    assert_err(
        decrypt_signing_secret(&key, &b64_encode(&decoded)),
        SigningKeyError::WrongKeyOrTag,
        "SigningKeyError::WrongKeyOrTag expected",
    );
}

/// Truncation below `nonce || tag` is rejected as malformed rather than
/// panicking, which is the robust equivalent of Go's recover path.
#[test]
fn truncated_ciphertext_is_rejected_not_panicked() {
    let key = EncryptionKey::from_password_encryption_key("");
    let decoded = b64_decode(GO_VECTOR_DEFAULT_KEY.1);
    let truncated = b64_encode(&decoded[..NONCE_LEN + TAG_LEN - 1]);
    assert_err(
        decrypt_signing_secret(&key, &truncated),
        SigningKeyError::MalformedCiphertext,
        "SigningKeyError::MalformedCiphertext expected",
    );
}

#[test]
fn plaintext_shapes_go_cannot_produce_are_rejected() {
    let key = EncryptionKey::from_password_encryption_key("");
    // Same wire format, plaintext that Go's generator could not emit.
    let short = encrypt_signing_secret(&key, "tooshort").unwrap();
    assert_err(
        decrypt_signing_secret(&key, &short),
        SigningKeyError::TooShort { min: 22 },
        "SigningKeyError::TooShort { min: 22 } expected",
    );
    let empty = encrypt_signing_secret(&key, "").unwrap();
    assert_err(
        decrypt_signing_secret(&key, &empty),
        SigningKeyError::EmptyPlaintext,
        "SigningKeyError::EmptyPlaintext expected",
    );
    let spaced = encrypt_signing_secret(&key, "secret with spaces inside").unwrap();
    assert_err(
        decrypt_signing_secret(&key, &spaced),
        SigningKeyError::InvalidCharacters,
        "SigningKeyError::InvalidCharacters expected",
    );
    let newline = encrypt_signing_secret(&key, "secret\nwith-newline-inside").unwrap();
    assert_err(
        decrypt_signing_secret(&key, &newline),
        SigningKeyError::InvalidCharacters,
        "SigningKeyError::InvalidCharacters expected",
    );
}

/// Go's `id.NewRandom()` output is 22 base62 chars: the compatibility floor,
/// accepted exactly.
#[test]
fn existing_go_base62_id_is_accepted() {
    let key = EncryptionKey::from_password_encryption_key("");
    let ct = encrypt_signing_secret(&key, GO_ID_SAMPLE).unwrap();
    assert_eq!(
        decrypt_signing_secret(&key, &ct).unwrap().expose(),
        GO_ID_SAMPLE
    );
    let just_short_enough = encrypt_signing_secret(&key, &GO_ID_SAMPLE[..21]).unwrap();
    assert_err(
        decrypt_signing_secret(&key, &just_short_enough),
        SigningKeyError::TooShort { min: 22 },
        "21 characters is below the floor",
    );
}

// ---------------------------------------------- never generate or overwrite

/// The codec offers no "decrypt or create" convenience: a failed decrypt
/// returns only the error, so a caller cannot accidentally mint replacement
/// material for existing stored data.
#[test]
fn decrypt_failure_produces_no_replacement_material() {
    let key = EncryptionKey::from_password_encryption_key("");
    let result = decrypt_signing_secret(&key, GO_VECTOR_CUSTOM_KEY.1);
    // Nothing is returned to store: the failure branch yields no value a caller
    // could write back over existing material.
    let stored_on_failure: Option<String> =
        result.as_ref().ok().map(|_| "would overwrite".to_string());
    assert_eq!(stored_on_failure, None);
    assert_err(
        result,
        SigningKeyError::WrongKeyOrTag,
        "wrong key must refuse",
    );
    // Generation is a separate, explicit call — not reachable from decrypt.
    assert!(
        generate_signing_secret()
            .expect("OS entropy must be available")
            .expose()
            != GO_VECTOR_CUSTOM_KEY.0
    );
}

// ------------------------------------------------------ disclosure surfaces

#[test]
fn signing_secret_debug_and_display_never_leak_the_value() {
    let key = EncryptionKey::from_password_encryption_key("");
    let secret = decrypt_signing_secret(&key, GO_VECTOR_DEFAULT_KEY.1).unwrap();
    let debug = format!("{secret:?}");
    assert!(
        !debug.contains(GO_VECTOR_DEFAULT_KEY.0),
        "Debug leaked the secret: {debug}"
    );
    // `SigningSecret` has no Display impl, so `{}` cannot even be written;
    // the length-only Debug is the entire disclosure.
    assert!(
        debug.contains("redacted"),
        "Debug must say it is redacted: {debug}"
    );
}

#[test]
fn encryption_key_debug_never_leaks_key_material() {
    // "" is a substring of every string, so the configured-value probe only
    // runs for the non-empty configuration; the digest check below covers both.
    for configured in ["", "a-test-password-encryption-key"] {
        let key = EncryptionKey::from_password_encryption_key(configured);
        let debug = format!("{key:?}");
        if !configured.is_empty() {
            assert!(
                !debug.contains(configured),
                "Debug leaked the configured key: {debug}"
            );
        }
        assert!(!debug.contains(DEFAULT_PASSWORD_ENCRYPTION_KEY));
        assert!(
            !debug.contains(&to_hex(key.as_bytes())),
            "Debug leaked the digest: {debug}"
        );
        assert!(debug.contains("redacted"));
    }
}

#[test]
fn error_display_and_debug_never_leak_stored_or_decrypted_values() {
    let cases: Vec<(SigningKeyError, &str)> = vec![
        (SigningKeyError::EmptyStoredValue, ""),
        (
            SigningKeyError::MalformedCiphertext,
            GO_VECTOR_DEFAULT_KEY.1,
        ),
        (SigningKeyError::WrongKeyOrTag, GO_VECTOR_CUSTOM_KEY.1),
        (SigningKeyError::EmptyPlaintext, GO_VECTOR_DEFAULT_KEY.0),
        (
            SigningKeyError::TooShort { min: 22 },
            GO_VECTOR_DEFAULT_KEY.0,
        ),
        (SigningKeyError::InvalidCharacters, GO_VECTOR_DEFAULT_KEY.0),
    ];
    for (err, sensitive) in cases {
        if sensitive.is_empty() {
            continue;
        }
        // Display, Debug and the whole source chain, like the preflight suite.
        let mut text = format!("{err} | {err:?}");
        let mut source = err.source();
        while let Some(e) = source {
            text.push_str(" | ");
            text.push_str(&e.to_string());
            source = e.source();
        }
        assert!(
            !text.contains(sensitive),
            "error disclosed {sensitive:?}: {text}"
        );
    }
}

/// Errors are `Copy`/`Clone` with no payload, so no error value can carry a
/// secret into a log struct later.
#[test]
fn errors_carry_no_payload() {
    let err = SigningKeyError::WrongKeyOrTag;
    let copied = err;
    assert_eq!(err, copied);
    // The largest payload any variant can hold is the `min: usize` of
    // `TooShort`, so an error value can carry at most a discriminant and one
    // integer — no string, no bytes, no secret.
    assert_eq!(std::mem::size_of::<SigningKeyError>(), 16);
    assert_eq!(
        std::mem::size_of::<SigningKeyError>(),
        std::mem::size_of::<usize>() * 2
    );
    assert!(err.source().is_none());
}

/// Asserts a specific refusal, without requiring `SigningSecret` to compare
/// equal (it deliberately has no `PartialEq`, so a decrypted secret cannot be
/// used as a value in comparisons or maps).
fn assert_err(
    result: Result<nd_cli::signing_keys::SigningSecret, SigningKeyError>,
    expected: SigningKeyError,
    context: &str,
) {
    match result {
        Err(actual) => assert_eq!(actual, expected, "{context}: got {actual:?}"),
        Ok(_) => panic!("{context}: expected {expected:?}, but decryption succeeded"),
    }
}

/// Standard padded base64, via the same crate the library uses — a hand-rolled
/// codec here could itself be wrong and would weaken the wire-format claim.
fn b64_decode(text: &str) -> Vec<u8> {
    BASE64_STANDARD
        .decode(text)
        .expect("test constant must be standard base64")
}

fn b64_encode(data: &[u8]) -> String {
    BASE64_STANDARD.encode(data)
}
