use diesel::{dsl, ExpressionMethods, JoinOnDsl, NullableExpressionMethods, QueryDsl, RunQueryDsl};
use rand::{rngs::OsRng, Rng};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::db::{DaoError, DbThreadPool};
use crate::models::blacklisted_token::NewBlacklistedToken;
use crate::models::user_otp::{NewUserOtp, UserOtp};
use crate::request_io::OutputSigninNonceAndHashParams;
use crate::schema::blacklisted_tokens as blacklisted_token_fields;
use crate::schema::blacklisted_tokens::dsl::blacklisted_tokens;
use crate::schema::signin_nonces as signin_nonce_fields;
use crate::schema::signin_nonces::dsl::signin_nonces;
use crate::schema::user_otps as user_otp_fields;
use crate::schema::user_otps::dsl::user_otps;
use crate::schema::user_security_data as user_security_data_fields;
use crate::schema::user_security_data::dsl::user_security_data;
use crate::schema::users as user_fields;
use crate::schema::users::dsl::users;

pub struct UserAuthStringHashAndStatus {
    pub user_id: Uuid,
    pub is_user_verified: bool,
    pub auth_string_hash: String,
}

pub struct Dao {
    db_thread_pool: DbThreadPool,
}

impl Dao {
    pub fn new(db_thread_pool: &DbThreadPool) -> Self {
        Self {
            db_thread_pool: db_thread_pool.clone(),
        }
    }

    pub fn get_user_auth_string_hash_and_status(
        &mut self,
        user_email: &str,
    ) -> Result<UserAuthStringHashAndStatus, DaoError> {
        let mut db_connection = self.db_thread_pool.get()?;

        let hash_and_status = db_connection
            .build_transaction()
            .run::<_, diesel::result::Error, _>(|conn| {
                let (user_id, is_user_verified, auth_string_hash) = users
                    .left_join(
                        user_security_data
                            .on(user_security_data_fields::user_id.eq(user_fields::id)),
                    )
                    .select((
                        user_fields::id,
                        user_fields::is_verified,
                        user_security_data_fields::auth_string_hash.nullable(),
                    ))
                    .filter(user_fields::email.eq(user_email))
                    .get_result::<(Uuid, bool, Option<String>)>(conn)?;

                if !is_user_verified {
                    return Ok(UserAuthStringHashAndStatus {
                        user_id,
                        is_user_verified,
                        auth_string_hash: String::new(),
                    });
                }

                let auth_string_hash = auth_string_hash.unwrap_or(String::new());

                if auth_string_hash.is_empty() {
                    return Err(diesel::result::Error::NotFound);
                }

                Ok(UserAuthStringHashAndStatus {
                    user_id,
                    is_user_verified,
                    auth_string_hash,
                })
            })?;

        Ok(hash_and_status)
    }

    pub fn blacklist_token(
        &mut self,
        token_signature: &[u8],
        token_expiration: u64,
    ) -> Result<(), DaoError> {
        let token_expiration = UNIX_EPOCH + Duration::from_secs(token_expiration);

        let blacklisted_token = NewBlacklistedToken {
            token_signature,
            token_expiration,
        };

        dsl::insert_into(blacklisted_tokens)
            .values(&blacklisted_token)
            .execute(&mut self.db_thread_pool.get()?)?;

        Ok(())
    }

    pub fn check_is_token_on_blacklist_and_blacklist(
        &mut self,
        token_signature: &[u8],
        token_expiration: u64,
    ) -> Result<bool, DaoError> {
        let count = blacklisted_tokens
            .filter(blacklisted_token_fields::token_signature.eq(token_signature))
            .count()
            .get_result::<i64>(&mut self.db_thread_pool.get()?)?;

        if count > 0 {
            Ok(true)
        } else {
            let token_expiration = UNIX_EPOCH + Duration::from_secs(token_expiration);

            let blacklisted_token = NewBlacklistedToken {
                token_signature,
                token_expiration,
            };

            dsl::insert_into(blacklisted_tokens)
                .values(&blacklisted_token)
                .execute(&mut self.db_thread_pool.get()?)?;

            Ok(false)
        }
    }

    pub fn get_otp(&mut self, user_id: Uuid) -> Result<UserOtp, DaoError> {
        Ok(user_otps
            .find(user_id)
            .get_result(&mut self.db_thread_pool.get()?)?)
    }

    pub fn save_otp(
        &mut self,
        otp: &str,
        user_id: Uuid,
        expiration: SystemTime,
    ) -> Result<(), DaoError> {
        let new_otp = NewUserOtp {
            user_id,
            otp,
            expiration,
        };

        dsl::insert_into(user_otps)
            .values(&new_otp)
            .on_conflict(user_otp_fields::user_id)
            .do_update()
            .set((
                user_otp_fields::otp.eq(otp),
                user_otp_fields::expiration.eq(expiration),
            ))
            .execute(&mut self.db_thread_pool.get()?)?;

        Ok(())
    }

    pub fn delete_otp(&mut self, user_id: Uuid) -> Result<(), DaoError> {
        dsl::delete(user_otps.find(user_id)).execute(&mut self.db_thread_pool.get()?)?;
        Ok(())
    }

    pub fn delete_all_expired_otps(&mut self) -> Result<(), DaoError> {
        dsl::delete(user_otps.filter(user_otp_fields::expiration.lt(SystemTime::now())))
            .execute(&mut self.db_thread_pool.get()?)?;

        Ok(())
    }

    pub fn clear_all_expired_tokens(&mut self) -> Result<usize, DaoError> {
        // Add two minutes to current time to prevent slight clock differences/inaccuracies from
        // opening a window for an attacker to use an expired refresh token
        Ok(diesel::delete(
            blacklisted_tokens
                .filter(blacklisted_token_fields::token_expiration.lt(SystemTime::now())),
        )
        .execute(&mut self.db_thread_pool.get()?)?)
    }

    pub fn get_and_refresh_signin_nonce(&mut self, user_email: &str) -> Result<i32, DaoError> {
        let mut db_connection = self.db_thread_pool.get()?;

        let nonce = db_connection
            .build_transaction()
            .run::<_, diesel::result::Error, _>(|conn| {
                let nonce = signin_nonces
                    .select(signin_nonce_fields::nonce)
                    .find(user_email)
                    .get_result::<i32>(conn)?;

                dsl::update(signin_nonces.find(user_email))
                    .set(signin_nonce_fields::nonce.eq(OsRng.gen::<i32>()))
                    .execute(conn)?;

                Ok(nonce)
            })?;

        Ok(nonce)
    }

    pub fn get_auth_string_data_signin_nonce(
        &mut self,
        user_email: &str,
    ) -> Result<OutputSigninNonceAndHashParams, DaoError> {
        let (salt, mem_cost, parallel, iters, nonce) = user_security_data
            .left_join(users.on(user_fields::id.eq(user_security_data_fields::user_id)))
            .left_join(signin_nonces.on(signin_nonce_fields::user_email.eq(user_fields::email)))
            .filter(user_fields::email.eq(user_email))
            .select((
                user_security_data_fields::auth_string_salt,
                user_security_data_fields::auth_string_memory_cost_kib,
                user_security_data_fields::auth_string_parallelism_factor,
                user_security_data_fields::auth_string_iters,
                signin_nonce_fields::nonce.nullable(),
            ))
            .first::<(Vec<u8>, i32, i32, i32, Option<i32>)>(&mut self.db_thread_pool.get()?)?;

        if let Some(n) = nonce {
            Ok(OutputSigninNonceAndHashParams {
                auth_string_salt: salt,
                auth_string_memory_cost_kib: mem_cost,
                auth_string_parallelism_factor: parallel,
                auth_string_iters: iters,
                nonce: n,
            })
        } else {
            Err(DaoError::QueryFailure(diesel::result::Error::NotFound))
        }
    }
}