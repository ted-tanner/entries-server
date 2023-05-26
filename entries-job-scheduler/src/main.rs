#[macro_use]
extern crate lazy_static;

use entries_utils::db::job_registry::Dao as JobRegistryDao;

use entries_utils::models::job_registry_item::JobRegistryItem;
use flexi_logger::{
    Age, Cleanup, Criterion, Duplicate, FileSpec, LogSpecification, Logger, Naming, WriteMode,
};
use std::time::{Duration, SystemTime};

mod env;
mod jobs;
mod runner;

use jobs::{
    ClearExpiredBudgetInvitesJob, ClearOldUserDeletionRequestsJob, ClearThrottleTableJob,
    ClearUnverifiedUsersJob, DeleteUsersJob, UnblacklistExpiredTokensJob,
};

fn main() {
    let mut conf_file_path: Option<String> = None;
    let mut args = std::env::args();

    // Eat the first argument, which is the relative path to the executable
    args.next();

    while let Some(arg) = args.next() {
        match arg.to_lowercase().as_str() {
            "--config" => {
                conf_file_path = {
                    let next_arg = args.next();

                    match next_arg {
                        Some(p) => Some(p),
                        None => {
                            eprintln!(
                                "ERROR: --config option specified but no config file path was given",
                            );
                            std::process::exit(1);
                        }
                    }
                };

                continue;
            }
            a => {
                eprintln!("ERROR: Invalid argument: {}", &a);
                std::process::exit(1);
            }
        }
    }

    env::initialize(&conf_file_path.unwrap_or(String::from("conf/jobs-conf.toml")));

    Logger::with(LogSpecification::info())
        .log_to_file(FileSpec::default().directory("./logs"))
        .rotate(
            Criterion::Age(Age::Day),
            Naming::Timestamps,
            Cleanup::KeepLogAndCompressedFiles(60, 365),
        )
        .cleanup_in_background_thread(true)
        .duplicate_to_stdout(Duplicate::All)
        .write_mode(WriteMode::BufferAndFlush)
        .format(|writer, now, record| {
            write!(
                writer,
                "{:5} | {} | {}:{} | {}",
                record.level(),
                now.format("%Y-%m-%dT%H:%M:%S%.6fZ"),
                record.module_path().unwrap_or("<unknown>"),
                record.line().unwrap_or(0),
                record.args()
            )
        })
        .use_utc()
        .start()
        .expect("Failed to starer");

    let mut registry_dao = JobRegistryDao::new(&env::db::DB_THREAD_POOL);
    let registry = registry_dao
        .get_all_jobs()
        .expect("Failed to obtain job registry");

    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(
            env::CONF
                .runner
                .worker_threads
                .unwrap_or(num_cpus::get() + 1),
        )
        .max_blocking_threads(env::CONF.runner.max_blocking_threads.unwrap_or(512))
        .enable_all()
        .build()
        .expect("Failed to launch asynchronous runtime")
        .block_on(async move {
            let mut job_runner = env::runner::JOB_RUNNER.lock().await;

            job_runner.register(Box::new(ClearExpiredBudgetInvitesJob::new(
                Duration::from_secs(
                    env::CONF
                        .clear_expired_budget_invites_job
                        .job_frequency_secs,
                ),
                get_last_run_time(ClearExpiredBudgetInvitesJob::name(), &registry),
                env::db::DB_THREAD_POOL.clone(),
            )));

            job_runner.register(Box::new(ClearOldUserDeletionRequestsJob::new(
                Duration::from_secs(
                    env::CONF
                        .clear_old_user_deletion_requests_job
                        .job_frequency_secs,
                ),
                get_last_run_time(ClearOldUserDeletionRequestsJob::name(), &registry),
                env::db::DB_THREAD_POOL.clone(),
            )));

            job_runner.register(Box::new(ClearThrottleTableJob::new(
                Duration::from_secs(env::CONF.clear_throttle_table_job.job_frequency_secs),
                get_last_run_time(ClearThrottleTableJob::name(), &registry),
                env::db::DB_THREAD_POOL.clone(),
            )));

            job_runner.register(Box::new(ClearUnverifiedUsersJob::new(
                Duration::from_secs(env::CONF.clear_unverified_users_job.job_frequency_secs),
                Duration::from_secs(
                    env::CONF
                        .clear_unverified_users_job
                        .max_unverified_user_age_days
                        * 24
                        * 60
                        * 60,
                ),
                get_last_run_time(ClearUnverifiedUsersJob::name(), &registry),
                env::db::DB_THREAD_POOL.clone(),
            )));

            job_runner.register(Box::new(DeleteUsersJob::new(
                Duration::from_secs(env::CONF.delete_users_job.job_frequency_secs),
                get_last_run_time(DeleteUsersJob::name(), &registry),
                env::db::DB_THREAD_POOL.clone(),
            )));

            job_runner.register(Box::new(UnblacklistExpiredTokensJob::new(
                Duration::from_secs(env::CONF.unblacklist_expired_tokens_job.job_frequency_secs),
                get_last_run_time(UnblacklistExpiredTokensJob::name(), &registry),
                env::db::DB_THREAD_POOL.clone(),
            )));

            job_runner.start().await;
        });
}

#[inline]
fn get_last_run_time(job_name: &str, registry: &[JobRegistryItem]) -> SystemTime {
    if let Some(t) = registry.iter().find(|&t| t.job_name == job_name) {
        t.last_run_timestamp
    } else {
        SystemTime::now()
    }
}
