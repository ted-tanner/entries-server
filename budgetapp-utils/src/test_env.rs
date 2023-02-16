use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Read;
use std::sync::Mutex;

#[derive(Deserialize, Serialize)]
pub struct Conf {
    pub database_uri: String,
    pub max_db_connections: Option<u32>,
}

lazy_static! {
    pub static ref CONF: Conf = build_conf();
    pub static ref CSPRNG: Mutex<ChaCha20Rng> = Mutex::new(ChaCha20Rng::from_entropy());
}

fn build_conf() -> Conf {
    const CONF_FILE_PATH: &str = "test-conf.toml";

    let mut conf_file = File::open(CONF_FILE_PATH).unwrap_or_else(|_| {
        eprintln!("ERROR: Expected configuration file at '{CONF_FILE_PATH}'");
        std::process::exit(1);
    });

    let mut contents = String::new();
    conf_file.read_to_string(&mut contents).unwrap_or_else(|_| {
        eprintln!(
            "ERROR: Configuration file at '{CONF_FILE_PATH}' should be a text file in the TOML format."
        );
        std::process::exit(1);
    });

    match toml::from_str::<Conf>(&contents) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("ERROR: Parsing '{CONF_FILE_PATH}' failed: {e}");
            std::process::exit(1);
        }
    }
}

pub mod db {
    use diesel::pg::PgConnection;
    use diesel::r2d2::ConnectionManager;

    use crate::db::create_db_thread_pool;

    type DbThreadPool = diesel::r2d2::Pool<ConnectionManager<PgConnection>>;

    lazy_static! {
        pub static ref DB_THREAD_POOL: DbThreadPool = create_db_thread_pool(
            crate::test_env::CONF.database_uri.as_str(),
            crate::test_env::CONF.max_db_connections,
        );
    }
}
