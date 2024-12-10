use log::*;
use std::path::PathBuf;
use thiserror::Error;

use crate::app_state::{build_objectstore, AppState, AppStateArgs};
use crate::auth::jwt::{JwtHelper, JwtHelperError};
use crate::config::load_or_init_config_textproto;
use crate::constants::USER_TABLE;
use crate::migrations::{apply_logs_migrations, apply_main_migrations};
use crate::rand::generate_random_string;
use crate::server::DataDir;
use crate::table_metadata::TableMetadataCache;

#[derive(Debug, Error)]
pub enum InitError {
  #[error("TB SQLite error: {0}")]
  Sqlite(#[from] trailbase_sqlite::Error),
  #[error("Rusqlite error: {0}")]
  Rusqlite(#[from] rusqlite::Error),
  #[error("RusqliteFromSql error: {0}")]
  FromSql(#[from] rusqlite::types::FromSqlError),
  #[error("DB Migration error: {0}")]
  Migration(#[from] refinery::Error),
  #[error("IO error: {0}")]
  IO(#[from] std::io::Error),
  #[error("Config error: {0}")]
  Config(#[from] crate::config::ConfigError),
  #[error("JwtHelper error: {0}")]
  JwtHelper(#[from] JwtHelperError),
  #[error("CreateAdmin error: {0}")]
  CreateAdmin(String),
  #[error("Custom initializer error: {0}")]
  CustomInit(String),
  #[error("Table error: {0}")]
  TableError(#[from] crate::table_metadata::TableLookupError),
  #[error("Schema error: {0}")]
  SchemaError(#[from] trailbase_sqlite::schema::SchemaError),
  #[error("Script error: {0}")]
  ScriptError(String),
  #[error("ObjectStore error: {0}")]
  ObjectStore(#[from] object_store::Error),
}

#[derive(Default)]
pub struct InitArgs {
  pub dev: bool,
  pub js_runtime_threads: Option<usize>,
}

pub async fn init_app_state(
  data_dir: DataDir,
  public_dir: Option<PathBuf>,
  args: InitArgs,
) -> Result<(bool, AppState), InitError> {
  // First create directory structure.
  data_dir.ensure_directory_structure().await?;

  // Then open or init new databases.
  let logs_conn = {
    let mut conn = init_logs_db(&data_dir)?;
    apply_logs_migrations(&mut conn)?;
    trailbase_sqlite::AsyncConnection::from_conn(conn).await?
  };

  // Open or init the main db. Note that we derive whether a new DB was initialized based on
  // whether the V1 migration had to be applied. Should be fairly robust.
  let (conn, new_db) = {
    let data_dir = data_dir.clone();
    let new_db = {
      let mut conn = trailbase_sqlite::connect_sqlite(Some(data_dir.main_db_path()), None).unwrap();
      apply_main_migrations(&mut conn, Some(data_dir.migrations_path())).unwrap()
    };

    (
      trailbase_sqlite::Connection::from_conn(move || {
        let mut conn =
          trailbase_sqlite::connect_sqlite(Some(data_dir.main_db_path()), None).unwrap();
        let _new_db = apply_main_migrations(&mut conn, Some(data_dir.migrations_path())).unwrap();
        conn
      })
      .await?,
      new_db,
    )
  };

  let table_metadata = TableMetadataCache::new(conn.clone()).await?;

  // Read config or write default one.
  let config = load_or_init_config_textproto(&data_dir, &table_metadata).await?;

  debug!("Initializing JSON schemas from config");
  trailbase_sqlite::schema::set_user_schemas(
    config
      .schemas
      .iter()
      .filter_map(|s| {
        let Some(ref name) = s.name else {
          warn!("Schema config entry missing name: {s:?}");
          return None;
        };

        let Some(ref schema) = s.schema else {
          warn!("Schema config entry missing schema: {s:?}");
          return None;
        };

        let json = match serde_json::from_str(schema) {
          Ok(json) => json,
          Err(err) => {
            error!("Invalid schema config entry for '{name}': {err}");
            return None;
          }
        };

        return Some((name.clone(), json));
      })
      .collect(),
  )?;

  let jwt = JwtHelper::init_from_path(&data_dir).await?;

  // Init geoip if present.
  let geoip_db_path = data_dir.root().join("GeoLite2-Country.mmdb");
  if let Err(err) = trailbase_sqlite::geoip::load_geoip_db(geoip_db_path.clone()) {
    debug!("Failed to load maxmind geoip DB '{geoip_db_path:?}': {err}");
  }

  let object_store = build_objectstore(&data_dir, config.server.s3_storage_config.as_ref())?;

  // Write out the latest .js/.d.ts runtime files.
  #[cfg(feature = "v8")]
  crate::js::write_js_runtime_files(&data_dir).await;

  let app_state = AppState::new(AppStateArgs {
    data_dir: data_dir.clone(),
    public_dir,
    dev: args.dev,
    table_metadata,
    config,
    conn,
    logs_conn,
    jwt,
    object_store,
    js_runtime_threads: args.js_runtime_threads,
  });

  if new_db {
    let num_admins: i64 = crate::util::query_one_row(
      app_state.user_conn(),
      &format!("SELECT COUNT(*) FROM {USER_TABLE} WHERE admin = TRUE"),
      (),
    )
    .await?
    .get(0)?;

    if num_admins == 0 {
      let email = "admin@localhost".to_string();
      let password = generate_random_string(20);

      app_state
        .user_conn()
        .execute(
          &format!(
            r#"
        INSERT INTO {USER_TABLE}
          (email, password_hash, verified, admin)
        VALUES
          ('{email}', (hash_password('{password}')), TRUE, TRUE);
        INSERT INTO
        "#
          ),
          (),
        )
        .await?;

      info!(
        "{}",
        indoc::formatdoc!(
          r#"
          Created new admin user:
              email: '{email}'
              password: '{password}'
        "#
        )
      );
    }
  }

  if cfg!(debug_assertions) {
    let text_config = app_state.get_config().to_text()?;
    debug!("Config: {text_config}");
  }

  return Ok((new_db, app_state));
}

fn init_logs_db(data_dir: &DataDir) -> Result<rusqlite::Connection, InitError> {
  let conn = trailbase_sqlite::connect_sqlite(data_dir.logs_db_path().into(), None)?;

  // Turn off secure_deletions, i.e. don't wipe the memory with zeros.
  conn
    .query_row("PRAGMA secure_delete = FALSE", (), |_row| Ok(()))
    .unwrap();

  // Sync less often
  conn.execute("PRAGMA synchronous = 1", ()).unwrap();

  return Ok(conn);
}
