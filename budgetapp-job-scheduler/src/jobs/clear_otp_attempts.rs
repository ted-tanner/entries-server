use budgetapp_utils::db::auth::Dao as AuthDao;
use budgetapp_utils::db::DbThreadPool;

use async_trait::async_trait;
use std::time::{Duration, SystemTime};

use crate::jobs::{Job, JobError};

pub struct ClearOtpAttemptsJob {
    pub job_frequency: Duration,
    pub attempts_lifetime: Duration,

    db_thread_pool: DbThreadPool,
    is_running: bool,
    last_run_time: SystemTime,
}

impl ClearOtpAttemptsJob {
    pub fn new(
        job_frequency: Duration,
        attempts_lifetime: Duration,
        db_thread_pool: DbThreadPool,
    ) -> Self {
        Self {
            job_frequency,
            attempts_lifetime,
            db_thread_pool,
            is_running: false,
            last_run_time: SystemTime::now(),
        }
    }
}

#[async_trait]
impl Job for ClearOtpAttemptsJob {
    fn name(&self) -> &'static str {
        "Clear OTP Attempts"
    }

    fn run_frequency(&self) -> Duration {
        self.job_frequency
    }

    fn last_run_time(&self) -> SystemTime {
        self.last_run_time
    }

    fn set_last_run_time(&mut self, time: SystemTime) {
        self.last_run_time = time
    }

    fn is_running(&self) -> bool {
        self.is_running
    }

    fn set_running_state_not_running(&mut self) {
        self.is_running = false;
    }

    fn set_running_state_running(&mut self) {
        self.is_running = true;
    }

    async fn run_handler_func(&mut self) -> Result<(), JobError> {
        let attempts_lifetime = self.attempts_lifetime;
        let mut dao = AuthDao::new(&self.db_thread_pool);

        tokio::task::spawn_blocking(move || dao.clear_otp_verification_count(attempts_lifetime))
            .await??;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use budgetapp_utils::db::user;
    use budgetapp_utils::models::otp_attempts::OtpAttempts;
    use budgetapp_utils::request_io::InputUser;
    use budgetapp_utils::schema::otp_attempts as otp_attempts_fields;
    use budgetapp_utils::schema::otp_attempts::dsl::otp_attempts;

    use diesel::{dsl, ExpressionMethods, QueryDsl, RunQueryDsl};
    use rand::Rng;
    use std::thread;

    use crate::env;

    #[test]
    fn test_last_run_time() {
        let before = SystemTime::now();

        thread::sleep(Duration::from_millis(1));
        let mut job = ClearOtpAttemptsJob::new(
            Duration::from_millis(1),
            Duration::from_millis(1),
            env::db::DB_THREAD_POOL.clone(),
        );
        thread::sleep(Duration::from_millis(1));

        assert!(job.last_run_time() > before);
        assert!(job.last_run_time() < SystemTime::now());

        let before = SystemTime::now();

        thread::sleep(Duration::from_millis(1));
        job.set_last_run_time(SystemTime::now());
        thread::sleep(Duration::from_millis(1));

        assert!(job.last_run_time() > before);
        assert!(job.last_run_time() < SystemTime::now());
    }

    #[tokio::test]
    #[ignore]
    async fn test_run_handler_fun() {
        let mut dao = AuthDao::new(&env::db::DB_THREAD_POOL);

        let mut user_ids = Vec::new();

        for _ in 0..3 {
            let user_number = rand::thread_rng().gen_range::<u128, _>(u128::MIN..u128::MAX);

            let new_user = InputUser {
                email: format!("test_user{}@test.com", &user_number),

                auth_string: String::new(),

                auth_string_salt: Vec::new(),
                auth_string_iters: 1000000,

                password_encryption_salt: Vec::new(),
                password_encryption_iters: 5000000,

                recovery_key_salt: Vec::new(),
                recovery_key_iters: 10000000,

                encryption_key_user_password_encrypted: Vec::new(),
                encryption_key_recovery_key_encrypted: Vec::new(),

                public_rsa_key: Vec::new(),
                private_rsa_key_encrypted: Vec::new(),

                preferences_encrypted: Vec::new(),
            };

            let user_id = user::Dao::new(&env::db::DB_THREAD_POOL)
                .create_user(new_user.clone(), "Test")
                .unwrap();

            user_ids.push(user_id);

            for _ in 0..rand::thread_rng().gen_range::<u32, _>(1..4) {
                dao.get_and_increment_otp_verification_count(user_id, Duration::from_millis(1))
                    .unwrap();
            }
        }

        let mut db_connection = env::db::DB_THREAD_POOL.get().unwrap();

        for user_id in &user_ids {
            let user_otp_attempts = otp_attempts
                .filter(otp_attempts_fields::user_id.eq(user_id))
                .first::<OtpAttempts>(&mut db_connection);
            assert!(user_otp_attempts.is_ok());
        }

        let mut job = ClearOtpAttemptsJob::new(
            Duration::from_secs(1),
            Duration::from_secs(1),
            env::db::DB_THREAD_POOL.clone(),
        );

        job.run_handler_func().await.unwrap();

        for user_id in &user_ids {
            let user_otp_attempts = otp_attempts
                .filter(otp_attempts_fields::user_id.eq(user_id))
                .first::<OtpAttempts>(&mut db_connection);
            assert!(user_otp_attempts.is_ok());
        }

        for user_id in &user_ids {
            dsl::update(otp_attempts.filter(otp_attempts_fields::user_id.eq(user_id)))
                .set(otp_attempts_fields::expiration_time.eq(SystemTime::now()
                    - Duration::from_secs(
                        env::CONF.clear_otp_attempts_job.attempts_lifetime_mins * 60 + 1,
                    )))
                .execute(&mut db_connection)
                .unwrap();
        }

        let mut job = ClearOtpAttemptsJob::new(
            Duration::from_millis(1),
            Duration::from_millis(1),
            env::db::DB_THREAD_POOL.clone(),
        );
        job.run_handler_func().await.unwrap();

        for user_id in user_ids {
            let user_otp_attempts = otp_attempts
                .filter(otp_attempts_fields::user_id.eq(user_id))
                .first::<OtpAttempts>(&mut db_connection);
            assert!(user_otp_attempts.is_err());
        }
    }
}
