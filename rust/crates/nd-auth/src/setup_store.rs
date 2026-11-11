//! Executing initial-admin setup against the SQL store.
//!
//! This wires the property/user portion of `server/initial_setup.go:19-73`;
//! the caller remains responsible for storing the music folder. The marker,
//! first-user check and admin insert share a retryable transaction. Unlike
//! Go, the dev password is hashed with argon2id before `UserRepo::put`.

use nd_db::execution::{execute, query_optional};
use nd_db::property_repo::{get_sql, put_sql};
use nd_db::store::{SQLStore, StoreError};
use nd_db::user_repo::{UserError, UserPutValues, UserRepo};

use crate::credentials::password::{hash_password, PasswordError};
use crate::initial_setup::{DEV_INITIAL_NAME, DEV_INITIAL_USER_NAME, INITIAL_SETUP_FLAG_KEY};

/// Initial setup storage or password-hashing error (`server/initial_setup.go:19-73`).
#[derive(Debug, thiserror::Error)]
pub enum SetupStoreError {
    /// Store lease, transaction or SQL failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Argon2id hashing failed.
    #[error(transparent)]
    Password(#[from] PasswordError),
}

fn sql_error(error: UserError) -> rusqlite::Error {
    match error {
        UserError::Sqlite(error) => error,
        UserError::NotFound => rusqlite::Error::QueryReturnedNoRows,
    }
}

/// Runs the property and dev-admin setup in one retryable store transaction
/// (`server/initial_setup.go:19-73`). Returns false if already completed.
/// `completed_at` is supplied by the caller and is also used for timestamps.
///
/// # Errors
/// Returns a store or argon2id error. A failed transaction commits no marker
/// or admin user.
pub async fn run_initial_setup(
    store: &SQLStore,
    dev_password: &str,
    completed_at: &str,
) -> Result<bool, SetupStoreError> {
    let hash = if dev_password.is_empty() {
        None
    } else {
        let cleartext = dev_password.to_owned();
        Some(
            store
                .with_connection(move |_| Ok(hash_password(&cleartext)))
                .await??,
        )
    };
    let completed_at = completed_at.to_owned();
    Ok(store
        .with_tx_retry(move |tx| {
            if query_optional(tx, &get_sql(INITIAL_SETUP_FLAG_KEY), |row| {
                row.get::<_, String>(0)
            })?
            .is_some()
            {
                return Ok(false);
            }
            let repo = UserRepo::new(tx);
            if let Some(hash) = hash.as_ref() {
                if repo
                    .count_by_username(DEV_INITIAL_USER_NAME)
                    .map_err(sql_error)?
                    == 0
                {
                    let values = UserPutValues {
                        id: "",
                        user_name: DEV_INITIAL_USER_NAME,
                        name: DEV_INITIAL_NAME,
                        email: "",
                        password: Some(hash),
                        is_admin: true,
                        last_login_at: None,
                        last_access_at: None,
                        created_at: &completed_at,
                        updated_at: &completed_at,
                        scrobble_filter: "",
                    };
                    repo.put(&values, &completed_at, false).map_err(sql_error)?;
                }
            }
            let statement = put_sql(INITIAL_SETUP_FLAG_KEY, &completed_at);
            if execute(tx, &statement.update)? == 0 {
                execute(tx, &statement.insert)?;
            }
            Ok(true)
        })
        .await?)
}
