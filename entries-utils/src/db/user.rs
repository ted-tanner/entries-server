use diesel::{dsl, ExpressionMethods, QueryDsl, RunQueryDsl};
use rand::{rngs::OsRng, Rng};
use sha1::{Digest, Sha1};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

use crate::db::{DaoError, DbThreadPool};
use crate::messages::UserPublicKey;
use crate::models::signin_nonce::NewSigninNonce;
use crate::models::user::NewUser;
use crate::models::user_backup_code::NewUserBackupCode;
use crate::models::user_deletion_request::{NewUserDeletionRequest, UserDeletionRequest};
use crate::models::user_deletion_request_budget_key::NewUserDeletionRequestBudgetKey;
use crate::models::user_keystore::NewUserKeystore;
use crate::models::user_preferences::NewUserPreferences;

use crate::schema::budget_access_keys as budget_access_key_fields;
use crate::schema::budget_access_keys::dsl::budget_access_keys;
use crate::schema::budgets::dsl::budgets;
use crate::schema::signin_nonces::dsl::signin_nonces;
use crate::schema::user_backup_codes::dsl::user_backup_codes;
use crate::schema::user_deletion_request_budget_keys as user_deletion_request_budget_key_fields;
use crate::schema::user_deletion_request_budget_keys::dsl::user_deletion_request_budget_keys;
use crate::schema::user_deletion_requests as user_deletion_request_fields;
use crate::schema::user_deletion_requests::dsl::user_deletion_requests;
use crate::schema::user_keystores as user_keystore_fields;
use crate::schema::user_keystores::dsl::user_keystores;
use crate::schema::user_preferences as user_preferences_fields;
use crate::schema::user_preferences::dsl::user_preferences;
use crate::schema::users as user_fields;
use crate::schema::users::dsl::users;

pub struct Dao {
    db_thread_pool: DbThreadPool,
}

impl Dao {
    pub fn new(db_thread_pool: &DbThreadPool) -> Self {
        Self {
            db_thread_pool: db_thread_pool.clone(),
        }
    }

    pub fn get_user_public_key(&self, user_email: &str) -> Result<UserPublicKey, DaoError> {
        let (key_id, key) = users
            .select((user_fields::public_key_id, user_fields::public_key))
            .filter(user_fields::email.eq(user_email))
            .first::<(Uuid, Vec<u8>)>(&mut self.db_thread_pool.get()?)?;

        Ok(UserPublicKey {
            id: key_id.into(),
            value: key,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_user(
        &self,
        email: &str,
        auth_string_hash: &str,
        auth_string_salt: &[u8],
        auth_string_memory_cost_kib: i32,
        auth_string_parallelism_factor: i32,
        auth_string_iters: i32,
        password_encryption_salt: &[u8],
        password_encryption_memory_cost_kib: i32,
        password_encryption_parallelism_factor: i32,
        password_encryption_iters: i32,
        recovery_key_salt: &[u8],
        recovery_key_memory_cost_kib: i32,
        recovery_key_parallelism_factor: i32,
        recovery_key_iters: i32,
        encryption_key_encrypted_with_password: &[u8],
        encryption_key_encrypted_with_recovery_key: &[u8],
        public_key_id: Uuid,
        public_key: &[u8],
        preferences_encrypted: &[u8],
        user_keystore_encrypted: &[u8],
        backup_codes: &[String],
    ) -> Result<Uuid, DaoError> {
        let current_time = SystemTime::now();
        let user_id = Uuid::new_v4();

        let email_lowercase = email.to_lowercase();

        let new_user = NewUser {
            id: user_id,
            email: &email_lowercase,
            is_verified: false,

            created_timestamp: current_time,

            public_key_id,
            public_key,

            auth_string_hash,
            auth_string_salt,
            auth_string_memory_cost_kib,
            auth_string_parallelism_factor,
            auth_string_iters,

            password_encryption_salt,
            password_encryption_memory_cost_kib,
            password_encryption_parallelism_factor,
            password_encryption_iters,

            recovery_key_salt,
            recovery_key_memory_cost_kib,
            recovery_key_parallelism_factor,
            recovery_key_iters,

            encryption_key_encrypted_with_password,
            encryption_key_encrypted_with_recovery_key,
        };

        let mut sha1_hasher = Sha1::new();
        sha1_hasher.update(preferences_encrypted);

        let new_user_preferences = NewUserPreferences {
            user_id,
            encrypted_blob: preferences_encrypted,
            encrypted_blob_sha1_hash: &sha1_hasher.finalize(),
        };

        let mut sha1_hasher = Sha1::new();
        sha1_hasher.update(user_keystore_encrypted);

        let new_user_keystore = NewUserKeystore {
            user_id,
            encrypted_blob: user_keystore_encrypted,
            encrypted_blob_sha1_hash: &sha1_hasher.finalize(),
        };

        let new_signin_nonce = NewSigninNonce {
            user_email: &email_lowercase,
            nonce: OsRng.gen(),
        };

        let backup_codes = backup_codes
            .iter()
            .map(|code| NewUserBackupCode { user_id, code })
            .collect::<Vec<_>>();

        let mut db_connection = self.db_thread_pool.get()?;

        db_connection
            .build_transaction()
            .run::<_, diesel::result::Error, _>(|conn| {
                dsl::insert_into(users).values(&new_user).execute(conn)?;

                dsl::insert_into(user_preferences)
                    .values(&new_user_preferences)
                    .execute(conn)?;

                dsl::insert_into(user_keystores)
                    .values(&new_user_keystore)
                    .execute(conn)?;

                dsl::insert_into(signin_nonces)
                    .values(&new_signin_nonce)
                    .execute(conn)?;

                dsl::insert_into(user_backup_codes)
                    .values(&backup_codes)
                    .execute(conn)?;

                Ok(())
            })?;

        Ok(user_id)
    }

    pub fn verify_user_creation(&self, user_id: Uuid) -> Result<(), DaoError> {
        dsl::update(users.find(user_id))
            .set(user_fields::is_verified.eq(true))
            .execute(&mut self.db_thread_pool.get()?)?;

        Ok(())
    }

    pub fn clear_unverified_users(
        &self,
        max_unverified_user_age: Duration,
    ) -> Result<(), DaoError> {
        diesel::delete(users.filter(user_fields::is_verified.eq(false)).filter(
            user_fields::created_timestamp.lt(SystemTime::now() - max_unverified_user_age),
        ))
        .execute(&mut self.db_thread_pool.get()?)?;

        Ok(())
    }

    pub fn rotate_user_public_key(
        &self,
        user_id: Uuid,
        public_key_id: Uuid,
        public_key: &[u8],
        expected_previous_public_key_id: Uuid,
    ) -> Result<(), DaoError> {
        let mut db_connection = self.db_thread_pool.get()?;

        db_connection
            .build_transaction()
            .run::<_, DaoError, _>(|conn| {
                let previous_public_key_id = users
                    .find(user_id)
                    .select(user_fields::public_key_id)
                    .get_result::<Uuid>(conn)?;

                if previous_public_key_id != expected_previous_public_key_id {
                    return Err(DaoError::OutOfDate);
                }

                dsl::update(users.find(user_id))
                    .set((
                        user_fields::public_key_id.eq(public_key_id),
                        user_fields::public_key.eq(public_key),
                    ))
                    .execute(conn)?;

                Ok(())
            })?;

        Ok(())
    }

    pub fn update_user_prefs(
        &self,
        user_id: Uuid,
        prefs_encrypted_blob: &[u8],
        expected_previous_data_hash: &[u8],
    ) -> Result<(), DaoError> {
        let mut db_connection = self.db_thread_pool.get()?;

        db_connection
            .build_transaction()
            .run::<_, DaoError, _>(|conn| {
                let previous_hash = user_preferences
                    .find(user_id)
                    .select(user_preferences_fields::encrypted_blob_sha1_hash)
                    .get_result::<Vec<u8>>(conn)?;

                if previous_hash != expected_previous_data_hash {
                    return Err(DaoError::OutOfDate);
                }

                let mut sha1_hasher = Sha1::new();
                sha1_hasher.update(prefs_encrypted_blob);

                dsl::update(user_preferences.find(user_id))
                    .set((
                        user_preferences_fields::encrypted_blob.eq(prefs_encrypted_blob),
                        user_preferences_fields::encrypted_blob_sha1_hash
                            .eq(sha1_hasher.finalize().as_slice()),
                    ))
                    .execute(conn)?;

                Ok(())
            })
    }

    pub fn update_user_keystore(
        &self,
        user_id: Uuid,
        keystore_encrypted_blob: &[u8],
        expected_previous_data_hash: &[u8],
    ) -> Result<(), DaoError> {
        let mut db_connection = self.db_thread_pool.get()?;

        db_connection
            .build_transaction()
            .run::<_, DaoError, _>(|conn| {
                let previous_hash = user_keystores
                    .find(user_id)
                    .select(user_keystore_fields::encrypted_blob_sha1_hash)
                    .get_result::<Vec<u8>>(conn)?;

                if previous_hash != expected_previous_data_hash {
                    return Err(DaoError::OutOfDate);
                }

                let mut sha1_hasher = Sha1::new();
                sha1_hasher.update(keystore_encrypted_blob);

                dsl::update(user_keystores.find(user_id))
                    .set((
                        user_keystore_fields::encrypted_blob.eq(keystore_encrypted_blob),
                        user_keystore_fields::encrypted_blob_sha1_hash
                            .eq(sha1_hasher.finalize().as_slice()),
                    ))
                    .execute(conn)?;

                Ok(())
            })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_password(
        &self,
        user_email: &str,
        new_auth_string_hash: &str,
        new_auth_string_salt: &[u8],
        new_auth_string_memory_cost_kib: i32,
        new_auth_string_parallelism_factor: i32,
        new_auth_string_iters: i32,
        new_password_encryption_salt: &[u8],
        new_password_encryption_memory_cost_kib: i32,
        new_password_encryption_parallelism_factor: i32,
        new_password_encryption_iters: i32,
        encrypted_encryption_key: &[u8],
    ) -> Result<(), DaoError> {
        dsl::update(users.filter(user_fields::email.eq(user_email)))
            .set((
                user_fields::auth_string_hash.eq(new_auth_string_hash),
                user_fields::auth_string_salt.eq(new_auth_string_salt),
                user_fields::auth_string_memory_cost_kib.eq(new_auth_string_memory_cost_kib),
                user_fields::auth_string_parallelism_factor.eq(new_auth_string_parallelism_factor),
                user_fields::auth_string_iters.eq(new_auth_string_iters),
                user_fields::password_encryption_salt.eq(new_password_encryption_salt),
                user_fields::password_encryption_memory_cost_kib
                    .eq(new_password_encryption_memory_cost_kib),
                user_fields::password_encryption_parallelism_factor
                    .eq(new_password_encryption_parallelism_factor),
                user_fields::password_encryption_iters.eq(new_password_encryption_iters),
                user_fields::encryption_key_encrypted_with_password.eq(encrypted_encryption_key),
            ))
            .execute(&mut self.db_thread_pool.get()?)?;

        Ok(())
    }

    pub fn update_recovery_key(
        &self,
        user_id: Uuid,
        new_recovery_key_salt: &[u8],
        new_recovery_key_memory_cost_kib: i32,
        new_recovery_key_parallelism_factor: i32,
        new_recovery_key_iters: i32,
        encrypted_encryption_key: &[u8],
    ) -> Result<(), DaoError> {
        dsl::update(users.find(user_id))
            .set((
                user_fields::recovery_key_salt.eq(new_recovery_key_salt),
                user_fields::recovery_key_memory_cost_kib.eq(new_recovery_key_memory_cost_kib),
                user_fields::recovery_key_parallelism_factor
                    .eq(new_recovery_key_parallelism_factor),
                user_fields::recovery_key_iters.eq(new_recovery_key_iters),
                user_fields::encryption_key_encrypted_with_recovery_key
                    .eq(encrypted_encryption_key),
            ))
            .execute(&mut self.db_thread_pool.get()?)?;

        Ok(())
    }

    pub fn save_user_deletion_budget_keys(
        &self,
        budget_access_key_ids: &[Uuid],
        user_id: Uuid,
        delete_me_time: SystemTime,
    ) -> Result<(), DaoError> {
        let deletion_request_budget_keys = budget_access_key_ids
            .iter()
            .map(|key_id| NewUserDeletionRequestBudgetKey {
                key_id: *key_id,
                user_id,
                delete_me_time,
            })
            .collect::<Vec<_>>();

        dsl::insert_into(user_deletion_request_budget_keys)
            .values(&deletion_request_budget_keys)
            .execute(&mut self.db_thread_pool.get()?)?;

        Ok(())
    }

    pub fn initiate_user_deletion(
        &self,
        user_id: Uuid,
        time_until_deletion: Duration,
    ) -> Result<(), DaoError> {
        let new_request = NewUserDeletionRequest {
            user_id,
            ready_for_deletion_time: SystemTime::now() + time_until_deletion,
        };

        dsl::insert_into(user_deletion_requests)
            .values(&new_request)
            .execute(&mut self.db_thread_pool.get()?)?;

        Ok(())
    }

    pub fn cancel_user_deletion(&self, user_id: Uuid) -> Result<(), DaoError> {
        diesel::delete(user_deletion_requests.find(user_id))
            .execute(&mut self.db_thread_pool.get()?)?;

        Ok(())
    }

    pub fn delete_user(&self, user_deletion_request: &UserDeletionRequest) -> Result<(), DaoError> {
        let mut db_connection = self.db_thread_pool.get()?;

        db_connection
            .build_transaction()
            .run::<_, diesel::result::Error, _>(|conn| {
                let budget_key_ids = user_deletion_request_budget_keys
                    .select(user_deletion_request_budget_key_fields::key_id)
                    .filter(
                        user_deletion_request_budget_key_fields::user_id
                            .eq(user_deletion_request.user_id),
                    )
                    .load::<Uuid>(conn)?;

                let budget_ids = diesel::delete(
                    budget_access_keys
                        .filter(budget_access_key_fields::key_id.eq_any(budget_key_ids)),
                )
                .returning(budget_access_key_fields::budget_id)
                .load::<Uuid>(conn)?;

                for budget_id in budget_ids {
                    let users_remaining_in_budget = budget_access_keys
                        .filter(budget_access_key_fields::budget_id.eq(budget_id))
                        .count()
                        .get_result::<i64>(conn)?;

                    if users_remaining_in_budget == 0 {
                        diesel::delete(budgets.find(budget_id)).execute(conn)?;
                    }
                }

                diesel::delete(users.find(user_deletion_request.user_id)).execute(conn)
            })?;

        Ok(())
    }

    pub fn get_all_users_ready_for_deletion(&self) -> Result<Vec<UserDeletionRequest>, DaoError> {
        Ok(user_deletion_requests
            .filter(user_deletion_request_fields::ready_for_deletion_time.lt(SystemTime::now()))
            .get_results(&mut self.db_thread_pool.get()?)?)
    }

    pub fn check_is_user_listed_for_deletion(&self, user_id: Uuid) -> Result<bool, DaoError> {
        Ok(
            dsl::select(dsl::exists(user_deletion_requests.find(user_id)))
                .get_result(&mut self.db_thread_pool.get()?)?,
        )
    }

    pub fn delete_old_user_deletion_requests(&self) -> Result<(), DaoError> {
        let mut db_connection = self.db_thread_pool.get()?;

        db_connection
            .build_transaction()
            .run::<_, diesel::result::Error, _>(|conn| {
                let user_ids = diesel::delete(user_deletion_request_budget_keys.filter(
                    user_deletion_request_budget_key_fields::delete_me_time.le(SystemTime::now()),
                ))
                .returning(user_deletion_request_budget_key_fields::user_id)
                .get_results::<Uuid>(conn)?;

                diesel::delete(
                    user_deletion_requests
                        .filter(user_deletion_request_fields::user_id.eq_any(user_ids)),
                )
                .execute(conn)?;

                Ok(())
            })?;

        Ok(())
    }
}
