# #S4 — Credential model (frozen contract: model B, clean break)

Status: **settled decision record** (issues.md Part A #S4). There is no open A-vs-B question.
Model A — the reversible AES-GCM password store and every credential arm that depends on it — is
**never ported**. Model B is the trunk credential model from day one.

Gated consumers (the tracker must block these until this contract is absorbed): #13, #14, #18,
#19, #24, #26, #27, #28, and the DB layer (#04/#05 read the `user`/`player`/`property` shapes
below). DB import issues #25/#29/#30/#31 are cancelled and own nothing.

## 1. Passwords: argon2id from day one

- `user.password` stores an argon2id hash. No plaintext, no reversible ciphertext, no legacy
  prefix sniffing. The Go credential switch had three arms — plaintext compare
  (`server/subsonic/middlewares.go:281` `pass != ""` arm: `decodePassword(pass) == user.Password`),
  the `enc:` hex-prefix arm (`server/subsonic/middlewares.go:243` `decodePassword`), and
  `t+s = md5(password+salt)` (`server/subsonic/middlewares.go:281` token arm) — **none of them is
  ported**. Verification is always argon2id verify.
- The AES-GCM reversible store (`utils/encrypt.go:15` `Encrypt`, `:40` `Decrypt`) keyed by
  `sha256(PasswordEncryptionKey | "just for obfuscation")` (`consts/consts.go:47-50`
  `DefaultEncryptionKey`, `:50` `PasswordsEncryptedKey`; `core/auth/auth.go:198-205` `getEncKey`,
  `createNewSecret`; `persistence/user_repository.go:389` `keyTo32Bytes`, `:394-450`
  `initPasswordEncryptionKey`, `:453` `encryptPassword`, `:464` `decryptPassword`) is **dropped
  entirely**. No Rust `encrypt`/`Decrypt` primitive for credentials exists.
- Rust crate guidance (issues.md #S4): use vetted crates — `argon2`, `jsonwebtoken`/`jose`,
  `rand`/`getrandom`. Nothing here is novel cryptography.
- No `migrate-credentials` command exists (Revision item 6 / #25 CANCELLED): the fresh service has
  no installs to migrate.

## 2. API keys: `nds_` prefix + unsalted SHA-256 lookup hash

- Keys are long random strings with prefix `nds_` (`consts/consts.go` `APIKeyPrefix`), format
  `^nds_[0-9A-Za-z]{22}$` (`persistence/player_repository.go:120`).
- Stored as `player.api_key_hash`, an unsalted SHA-256 hex hash — fast hash is acceptable for
  high-entropy random keys and keeps lookups indexed
  (`persistence/player_repository.go:194-199` `hashAPIKey`, lookup at `:201`).
- Column + unique index: `db/migrations/20260924010054_add_player_api_key_hash.sql`
  (`player.api_key_hash varchar default null`, `create unique index player_api_key_hash on
  player(api_key_hash)`). The fresh Rust migration carries this shape.
- Ownership semantics (`persistence/player_repository.go:37,39,49,154,218`): API key hashes are
  per-player; lookup also enforces the key's owner matches the requesting user
  (`server/subsonic/middlewares.go:235-239`).

## 3. JWT sessions: mint and accept `native` only

- Two signing secrets, on purpose (`core/auth/auth.go:23-29,50,172`):
  - **`TokenAuth`** signs UI/API session tokens; rotated so stale sessions die.
  - **`PublicTokenAuth`** signs public-link tokens (artwork, share streams) on a separate secret
    that survives rotation.
- Audiences: the Go set is `jellyfin|subsonic|native` (`core/auth/auth.go:33-36`). Per Revision
  (#25 CANCELLED) the Rust port mints and accepts **`native` only**; `subsonic` and `jellyfin`
  audiences are removed from the constant set.
- Secrets live in the `property` table under keys **`JWTSecret`** and **`JWTPublicSecret`**
  (`consts/consts.go:33-34`; `core/auth/auth.go:50-54` `Init`/`loadOrCreateSecret`). In the Rust
  port they are stored **plaintext** — the AES-GCM-at-rest wrapper (`createNewSecret` /
  `loadOrCreateSecret` decrypt path, `core/auth/auth.go:54-66,172-198`) is not ported. Missing
  secrets are generated (random, `id.NewRandom` equivalent) and stored at first boot.
- **`PasswordsEncryptedKey` is dead**: the `property` key (`consts/consts.go:50`) is not created
  by the fresh schema, and `PasswordEncryptionKey` config is dead and absent from the fresh TOML
  (see #S3).

## 4. Claim shape (byte-compatible with `core/auth/claims.go:11,31-70`)

`Claims` struct — standard: `Issuer` ("ND", `consts/consts.go` `JWTIssuer`), `Subject` (username
for session tokens), `Audience` (empty means any API), `IssuedAt`, `ExpiresAt`; custom claims:
`UserID` (`uid`), `IsAdmin` (`adm`), `ID` (`id` — artwork/mediafile ID), `Format` (`f`),
`BitRate` (`b`), `ShareID` (`sid`), `Epoch` (`ep` — the user's `token_epoch` at mint time).
`ToMap` emits only non-zero fields. Session expiry: `SessionTimeout` (default 48h,
`consts/consts.go` `DefaultSessionTimeout`); `TouchClaims` re-signs with a fresh expiry
(`core/auth/auth.go:110-114`); the refreshing-writer path re-stamps the epoch from context
(`server/auth.go:310-327`).

## 5. Revocation: `token_epoch`

- `user.token_epoch INTEGER NOT NULL DEFAULT 0`
  (`db/migrations/20260822062750_add_user_token_epoch.sql`).
- Epoch bump rides the password UPDATE as a single statement
  (`persistence/user_repository.go:129-146`): `UPDATE user SET ..., token_epoch = token_epoch + 1
  ... RETURNING token_epoch`, so a concurrent password change cannot leave a session valid that
  the other change should have revoked. New-user insert takes epoch 0.
- `CheckClaims` gates a verified token against the user it names
  (`core/auth/auth.go:139-158`): `uid` mismatch → `ErrWrongUser` (usernames can be reused: a
  deleted-then-recreated name yields a new id at epoch 0 that an old token must not match),
  `Epoch != user.TokenEpoch` → `ErrTokenRevoked`, audience not in `c.Audience` →
  `ErrWrongAudience`. Rust keeps the same three errors and the same precedence.

## 6. Public tokens: HMAC verify-not-decode (the Go bug is fixed, not copied)

- **Go bug** (`server/public/handle_images.go:76-93` `decodeArtworkID`): it calls
  `auth.PublicTokenAuth.Decode(tokenString)` — **decode only, no signature verification**. A
  forged token with a chosen `id` claim would be accepted for artwork lookup.
- **Rust contract**: every public-token consumer MUST call the verifying path
  (`ValidatePublic` semantics, `core/auth/auth.go:121-130`: `jwtauth.VerifyToken(PublicTokenAuth,
  …)`) — or its Rust equivalent — before reading any claim. A decode-without-verify helper must
  not exist in the API surface. After verification, the claim-handling logic is otherwise
  identical: require the `id` claim, else fall back to `mf-` + id for mediafile-share artwork
  tokens (`server/public/handle_images.go:88-93`).
- Mint side: `CreatePublicToken` / `CreateExpiringPublicToken` always set the `ND` issuer
  (`core/auth/auth.go:70-84`).

## 7. ExtAuth trusted sources

- `ExtAuth.UserHeader` username extraction only trusts the reverse-proxy IP when it matches
  `ExtAuth.TrustedSources`, a comma-separated CIDR list (`server/auth.go:212,418-427`
  `UsernameFromExtAuthHeader`, `validateIPAgainstList`).
- Unix-socket special case: remote address on a unix socket is `@` (golang/go#49825); trusted
  only when `Server.Address` starts with `unix:` and `@` appears in the list
  (`server/auth.go:427-431` region).
- Empty `TrustedSources` disables the header entirely.

## 8. Rate limits and body caps (not A-specific — must not be relaxed)

- Unauthenticated login routes cap the request body at **8 KiB** (`server/auth.go:100` —
  `MaxLoginBodySize = 8 << 10`, applied by `LimitLoginBody`).
- Login rate limiting: `AuthRequestLimit` / `AuthWindowLength` per-client-IP limiter, warn loudly
  when disabled (`server/server.go:212` auth route wiring).
- Failed Subsonic-login limiter: windowed per-key cap; admits up to `2*limit-1` guesses per
  window; keyed by hashed username so attacker-chosen names cannot bloat memory
  (`server/subsonic/auth_limiter.go:15` and struct doc).
- Form-body cap 10 MiB on the Subsonic form-parsing middleware
  (`server/subsonic/middlewares.go:34`). *(Applied to whatever request-parsing middleware the
  native API keeps; the Subsonic router itself is not ported — Revision item 2.)*

## 9. First-run bootstrap replaces any migration

- No upgrade path exists (Revision item 6). Initial setup follows `server/initial_setup.go`:
  create the music folder record, check/set the `InitialSetupFlagKey` property, optionally
  auto-create the dev admin (`admin` / `Dev Admin`, `consts/consts.go`) when
  `DevAutoCreateAdminPassword` is set. The first real admin is created through the normal
  `createAdmin` endpoint (unauthenticated only while zero admins exist — `server/server.go:212`
  wiring). There is no stored-ciphertext question to answer.

## 10. Explicitly not ported (checklist)

| Go artifact | Location | Disposition |
| --- | --- | --- |
| AES-GCM `Encrypt`/`Decrypt` | `utils/encrypt.go:15,40` | dropped; no credential at-rest encryption |
| `DefaultEncryptionKey` / `PasswordsEncryptedKey` | `consts/consts.go:47,50` | dead; not in fresh TOML or `property` set |
| `initPasswordEncryptionKey` re-encrypt pass | `persistence/user_repository.go:394-450` | dropped (no installs) |
| plaintext / `enc:` / `md5(t+s)` credential arms | `server/subsonic/middlewares.go:243,281` | dropped; argon2id only |
| `subsonicSalt`/`subsonicToken` claim payload | `server/auth.go:90-95` | dropped with the Subsonic surface |
| `subsonic`/`jellyfin` JWT audiences | `core/auth/auth.go:33-36` | removed; `native` only |
| decode-without-verify of public tokens | `server/public/handle_images.go:76-93` | **fixed**: verify-then-read |
| `migrate-credentials` command | (was #25) | cancelled; never exists |
