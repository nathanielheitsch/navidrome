//! The initial-setup flow — the port of `server/initial_setup.go`.
//!
//! Go source: `initial_setup.go:19-73` (`initialSetup`, run once inside
//! a transaction: store the music folder, then read the
//! `InitialSetup` property — present means already done — and only
//! then optionally create the dev admin) and `:38-73`
//! (`createInitialAdminUser`), pinned by `server/initial_setup_test.go`
//! and driven from `conf/testdata` config in production.
//!
//! The Go function is `log.Fatal`-driven; the port returns the error
//! and lets the caller decide process fate. The dev admin is created
//! only when a dev password is configured and the user table is
//! empty — the same first-user-is-admin discipline the headers login
//! applies (`server/auth.go` `handleLoginFromHeaders`).

use thiserror::Error;

/// `"InitialSetup"` — the property key marking setup as done
/// (`consts.InitialSetupFlagKey`, `consts/consts.go:15`).
pub const INITIAL_SETUP_FLAG_KEY: &str = "InitialSetup";

/// `"admin"` — the dev initial user name (`consts.DevInitialUserName`,
/// `consts/consts.go:54`).
pub const DEV_INITIAL_USER_NAME: &str = "admin";

/// `"Dev Admin"` — the dev initial display name
/// (`consts.DevInitialName`, `consts/consts.go:55`).
pub const DEV_INITIAL_NAME: &str = "Dev Admin";

/// The storage the initial-setup flow needs — the slice of
/// `model.DataStore` `initial_setup.go` uses: property get/put and user
/// count/find (`tx.Property()`, `tx.User()`).
pub trait InitialSetupStore {
    /// `Property().Get` — `Err` on a missing key, like Go's repository.
    fn property_get(&self, key: &str) -> Result<String, SetupError>;
    /// `Property().Put`.
    fn property_put(&self, key: &str, value: &str) -> Result<(), SetupError>;
    /// `User().CountAll` — the total number of users.
    fn user_count_all(&self) -> Result<u64, SetupError>;
    /// `User().Put` — store a new user.
    fn user_put(&self, user: &InitialAdminUser) -> Result<(), SetupError>;
}

/// The `model.User` fields `createInitialAdminUser` writes
/// (`initial_setup.go:55-63`).
// NO Debug derive: new_password is a cleartext dev password
// (security-reviewer #13 finding 2).
#[derive(Clone, PartialEq, Eq)]
pub struct InitialAdminUser {
    /// A fresh random id (`id.NewRandom()`).
    pub id: String,
    /// `consts.DevInitialUserName`.
    pub user_name: String,
    /// `consts.DevInitialName`.
    pub name: String,
    /// Empty in the Go flow.
    pub email: String,
    /// The dev password in plaintext in the Go dev flow (`NewPassword`).
    pub new_password: String,
    /// Always `true` for the dev admin.
    pub is_admin: bool,
}

/// The setup flow's errors: the wrapped storage failures plus the
/// already-done sentinel that `initialSetup` treats as a no-op
/// (`properties.Get` succeeding, `initial_setup.go:25-28`).
#[derive(Debug, Error, PartialEq, Clone)]
pub enum SetupError {
    /// Storage failed with the caller's message.
    #[error("storage error: {0}")]
    Storage(String),
    /// The `InitialSetup` property already exists — nothing to do.
    #[error("initial setup already completed")]
    AlreadyCompleted,
    /// Creating the initial admin user failed.
    #[error("could not create initial admin user: {0}")]
    CreateAdmin(String),
    /// The user table could not be read.
    #[error("could not access user table: {0}")]
    AccessUserTable(String),
}

/// `createInitialAdminUser` (`initial_setup.go:38-73`): if the user
/// table is empty, create the dev admin with `initial_password`.
/// A non-empty table is a no-op, and every storage failure is
/// propagated — never ignored.
pub fn create_initial_admin_user<S: InitialSetupStore>(
    store: &S,
    initial_password: &str,
) -> Result<(), SetupError> {
    let count = store
        .user_count_all()
        .map_err(|e| SetupError::AccessUserTable(e.to_string()))?;
    if count == 0 {
        // `id.NewRandom()` — a fresh random id for the new admin.
        let new_id =
            random_id().ok_or_else(|| SetupError::CreateAdmin("id entropy unavailable".into()))?;
        let user = InitialAdminUser {
            id: new_id,
            user_name: DEV_INITIAL_USER_NAME.to_string(),
            name: DEV_INITIAL_NAME.to_string(),
            email: String::new(),
            new_password: initial_password.to_string(),
            is_admin: true,
        };
        store
            .user_put(&user)
            .map_err(|e| SetupError::CreateAdmin(e.to_string()))?;
    }
    Ok(())
}

/// The flow decision `initialSetup` encodes (`initial_setup.go:19-37`),
/// transaction discipline excluded (the caller wraps the store).
/// The setup timestamp is supplied by the caller (`time.Now().String()`).
pub fn initial_setup<S: InitialSetupStore>(
    store: &S,
    dev_auto_create_admin_password: &str,
    completed_at: &str,
) -> Result<(), SetupError> {
    // Already-done check: a present flag key short-circuits (Go: the
    // `Get` err == nil branch returns nil inside the transaction).
    if store.property_get(INITIAL_SETUP_FLAG_KEY).is_ok() {
        return Ok(());
    }
    if !dev_auto_create_admin_password.is_empty() {
        create_initial_admin_user(store, dev_auto_create_admin_password)?;
    }
    store
        .property_put(INITIAL_SETUP_FLAG_KEY, completed_at)
        .map_err(|e| SetupError::Storage(e.to_string()))
}

/// `id.NewRandom()` — 16 random bytes, base62-shaped here as lowercase
/// hex, which keeps the id collision-free for the same purpose.
fn random_id() -> Option<String> {
    let mut buf = [0u8; 16];
    getrandom::getrandom(&mut buf).ok()?;
    let mut out = String::with_capacity(32);
    for b in buf {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// An in-memory stand-in for the mock datastore the Go test uses
    /// (`tests.MockDataStore`), with the same failure-injection points
    /// as `failingPutUserRepo`.
    #[derive(Default)]
    struct MemStore {
        properties: RefCell<Vec<(String, String)>>,
        users: RefCell<Vec<InitialAdminUser>>,
        fail_count_all: Option<SetupError>,
        fail_put: Option<SetupError>,
        fail_property_put: Option<SetupError>,
    }

    impl InitialSetupStore for MemStore {
        fn property_get(&self, key: &str) -> Result<String, SetupError> {
            self.properties
                .borrow()
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .ok_or(SetupError::Storage("not found".into()))
        }
        fn property_put(&self, key: &str, value: &str) -> Result<(), SetupError> {
            if let Some(e) = &self.fail_property_put {
                return Err(e.clone());
            }
            self.properties
                .borrow_mut()
                .push((key.to_string(), value.to_string()));
            Ok(())
        }
        fn user_count_all(&self) -> Result<u64, SetupError> {
            match &self.fail_count_all {
                Some(e) => Err(e.clone()),
                None => Ok(u64::try_from(self.users.borrow().len()).unwrap_or(0)),
            }
        }
        fn user_put(&self, user: &InitialAdminUser) -> Result<(), SetupError> {
            match &self.fail_put {
                Some(e) => Err(e.clone()),
                None => {
                    self.users.borrow_mut().push(user.clone());
                    Ok(())
                }
            }
        }
    }

    // Ported from server/initial_setup_test.go — same AAA discipline.

    #[test]
    fn creates_admin_with_password_when_table_empty() {
        // Arrange
        let ds = MemStore::default();
        // Act
        create_initial_admin_user(&ds, "pass123").unwrap();
        // Assert
        let users = ds.users.borrow();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].user_name, "admin");
        assert_eq!(users[0].name, "Dev Admin");
        assert_eq!(users[0].new_password, "pass123");
        assert!(users[0].is_admin);
        assert!(users[0].email.is_empty());
        assert_eq!(users[0].id.len(), 32);
    }

    #[test]
    fn does_not_create_admin_when_table_not_empty() {
        // Arrange
        let ds = MemStore::default();
        create_initial_admin_user(&ds, "first").unwrap();
        // Act
        create_initial_admin_user(&ds, "second").unwrap();
        // Assert: still exactly one user, still the first password.
        let users = ds.users.borrow();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].new_password, "first");
    }

    #[test]
    fn returns_error_when_user_cannot_be_stored() {
        // Arrange
        let ds = MemStore {
            fail_put: Some(SetupError::Storage("db is down".into())),
            ..MemStore::default()
        };
        // Act
        let err = create_initial_admin_user(&ds, "pass123").unwrap_err();
        // Assert
        assert!(matches!(err, SetupError::CreateAdmin(_)));
        assert_eq!(
            err.to_string(),
            "could not create initial admin user: storage error: db is down"
        );
    }

    #[test]
    fn returns_error_when_user_table_cannot_be_read() {
        // Arrange
        let ds = MemStore {
            fail_count_all: Some(SetupError::Storage("db is down".into())),
            ..MemStore::default()
        };
        // Act
        let err = create_initial_admin_user(&ds, "pass123").unwrap_err();
        // Assert
        assert!(matches!(err, SetupError::AccessUserTable(_)));
    }

    // The initialSetup flow itself (server/initial_setup.go:19-37),
    // driven the way production drives it: config from conf/testdata
    // shapes (`DevAutoCreateAdminPassword`, an optional dev-only knob).
    #[test]
    fn flow_runs_setup_when_flag_absent_and_password_configured() {
        // Arrange
        let ds = MemStore::default();
        // Act
        initial_setup(&ds, "devpassword", "2026-11-04").unwrap();
        // Assert: the dev admin was created…
        assert_eq!(ds.users.borrow().len(), 1);
        // …and the completion flag was stored.
        assert_eq!(
            ds.property_get(INITIAL_SETUP_FLAG_KEY).unwrap(),
            "2026-11-04"
        );
    }

    #[test]
    fn flow_skips_admin_creation_when_no_password_configured() {
        // Arrange: the default config state — the dev knob empty, as
        // in conf/testdata/cfg.toml (no DevAutoCreateAdminPassword key).
        let ds = MemStore::default();
        // Act
        initial_setup(&ds, "", "2026-11-04").unwrap();
        // Assert: flag stored, no users created.
        assert_eq!(ds.users.borrow().len(), 0);
        assert!(ds.property_get(INITIAL_SETUP_FLAG_KEY).is_ok());
    }

    #[test]
    fn flow_is_a_noop_when_flag_present() {
        // Arrange: already ran once.
        let ds = MemStore::default();
        initial_setup(&ds, "devpassword", "first-run").unwrap();
        let users_after_first = ds.users.borrow().len();
        // Act: a second boot must not create another admin or rewrite
        // the flag.
        initial_setup(&ds, "devpassword", "second-run").unwrap();
        // Assert
        assert_eq!(ds.users.borrow().len(), users_after_first);
        assert_eq!(
            ds.property_get(INITIAL_SETUP_FLAG_KEY).unwrap(),
            "first-run"
        );
    }

    #[test]
    fn flow_propagates_property_put_failure() {
        // Arrange
        let ds = MemStore {
            fail_property_put: Some(SetupError::Storage("db is down".into())),
            ..MemStore::default()
        };
        // Act
        let err = initial_setup(&ds, "", "2026-11-04").unwrap_err();
        // Assert: fail closed — the setup did not report success.
        assert!(matches!(err, SetupError::Storage(_)));
        assert_eq!(ds.users.borrow().len(), 0);
    }
}
