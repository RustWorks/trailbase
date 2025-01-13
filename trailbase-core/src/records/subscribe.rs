use axum::{
  extract::{Path, State},
  response::sse::{Event, KeepAlive, Sse},
  response::{IntoResponse, Response},
};
use futures::stream::StreamExt;
use parking_lot::RwLock;
use rusqlite::hooks::{Action, PreUpdateCase};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{
  atomic::{AtomicI64, Ordering},
  Arc,
};
use trailbase_sqlite::connection::{extract_record_values, extract_row_id};

use crate::auth::user::User;
use crate::records::sql_to_json::valueref_to_json;
use crate::records::RecordApi;
use crate::records::{Permission, RecordError};
use crate::table_metadata::{TableMetadata, TableMetadataCache};
use crate::value_notifier::Computed;
use crate::AppState;

static SUBSCRIPTION_COUNTER: AtomicI64 = AtomicI64::new(0);

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize)]
pub enum RecordAction {
  Delete,
  Insert,
  Update,
}

impl From<Action> for RecordAction {
  fn from(value: Action) -> Self {
    return match value {
      Action::SQLITE_DELETE => RecordAction::Delete,
      Action::SQLITE_INSERT => RecordAction::Insert,
      Action::SQLITE_UPDATE => RecordAction::Update,
      _ => unreachable!("{value:?}"),
    };
  }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DbEvent {
  Update(Option<serde_json::Value>),
  Insert(Option<serde_json::Value>),
  Delete(Option<serde_json::Value>),
  Error(String),
}

pub struct Subscription {
  /// Id uniquely identifying this subscription.
  subscription_id: i64,
  /// Name of the API this subscription is subscribed to. We need to lookup the Record API on the
  /// hot path to make sure we're getting the latest configuration.
  record_api_name: String,

  /// Record id present for subscriptions to specific records.
  // record_id: Option<trailbase_sqlite::Value>,
  user: Option<User>,
  /// Channel for sending events to the SSE handler.
  channel: async_channel::Sender<Event>,
}

/// Internal, shareable state of the cloneable SubscriptionManager.
struct ManagerState {
  /// SQLite connection to monitor.
  conn: trailbase_sqlite::Connection,
  /// Table metadata for mapping column indexes to column names needed for building JSON encoded
  /// records.
  table_metadata: TableMetadataCache,
  /// Record API configurations.
  record_apis: Computed<Vec<(String, RecordApi)>, crate::config::proto::Config>,

  /// Map from table name to row id to list of subscriptions.
  record_subscriptions: RwLock<HashMap<String, HashMap<i64, Vec<Subscription>>>>,

  /// Map from table name to table subscriptions.
  table_subscriptions: RwLock<HashMap<String, Vec<Subscription>>>,
}

impl ManagerState {
  fn lookup_record_api(&self, name: &str) -> Option<RecordApi> {
    for (record_api_name, record_api) in self.record_apis.load().iter() {
      if record_api_name == name {
        return Some(record_api.clone());
      }
    }
    return None;
  }
}

#[derive(Clone)]
pub struct SubscriptionManager {
  state: Arc<ManagerState>,
}

struct ContinuationState {
  state: Arc<ManagerState>,
  table_metadata: Option<Arc<TableMetadata>>,
  action: RecordAction,
  table_name: String,
  rowid: i64,
  record_values: Vec<rusqlite::types::Value>,
}

impl SubscriptionManager {
  pub fn new(
    conn: trailbase_sqlite::Connection,
    table_metadata: TableMetadataCache,
    record_apis: Computed<Vec<(String, RecordApi)>, crate::config::proto::Config>,
  ) -> Self {
    return Self {
      state: Arc::new(ManagerState {
        conn,
        table_metadata,
        record_apis,

        record_subscriptions: RwLock::new(HashMap::new()),
        table_subscriptions: RwLock::new(HashMap::new()),
      }),
    };
  }

  pub fn num_record_subscriptions(&self) -> usize {
    let mut count: usize = 0;
    for table in self.state.record_subscriptions.read().values() {
      for record in table.values() {
        count += record.len();
      }
    }
    return count;
  }

  fn broker_subscriptions(
    s: &ManagerState,
    conn: &rusqlite::Connection,
    subs: &[Subscription],
    record: &[(&str, rusqlite::types::ValueRef<'_>)],
    event: &Event,
  ) -> Vec<usize> {
    let mut dead_subscriptions: Vec<usize> = vec![];
    for (idx, sub) in subs.iter().enumerate() {
      let Some(api) = s.lookup_record_api(&sub.record_api_name) else {
        dead_subscriptions.push(idx);
        continue;
      };

      if let Err(_err) =
        api.check_record_level_read_access(conn, Permission::Read, record, sub.user.as_ref())
      {
        // This can happen if the record api configuration has changed since originally
        // subscribed. In this case we just send and error and cancel the subscription.
        if let Ok(ev) = Event::default().json_data(DbEvent::Error("Access denied".into())) {
          let _ = sub.channel.try_send(ev);
        }
        dead_subscriptions.push(idx);
        continue;
      }

      match sub.channel.try_send(event.clone()) {
        Ok(_) => {}
        Err(async_channel::TrySendError::Full(ev)) => {
          log::warn!("Channel full, dropping event: {ev:?}");
        }
        Err(async_channel::TrySendError::Closed(_ev)) => {
          dead_subscriptions.push(idx);
        }
      }
    }

    return dead_subscriptions;
  }

  /// Continuation of the preupdate hook being scheduled on the executor.
  fn hook_continuation(conn: &rusqlite::Connection, state: ContinuationState) {
    let ContinuationState {
      state,
      table_metadata,
      table_name,
      action,
      rowid,
      record_values,
    } = state;
    let s = &state;
    let table_name = table_name.as_str();

    // If table_metadata is missing, the config/schema must have changed, thus removing the
    // subscriptions.
    let Some(table_metadata) = table_metadata else {
      log::warn!("Table not found: {table_name}. Removing subscriptions");

      let mut record_subs = s.record_subscriptions.write();
      record_subs.remove(table_name);

      let mut table_subs = s.table_subscriptions.write();
      table_subs.remove(table_name);

      if record_subs.is_empty() && table_subs.is_empty() {
        conn.preupdate_hook(NO_HOOK);
      }

      return;
    };

    // Join values with column names.
    let record: Vec<(&str, rusqlite::types::ValueRef<'_>)> = record_values
      .iter()
      .enumerate()
      .map(|(idx, v)| (table_metadata.schema.columns[idx].name.as_str(), v.into()))
      .collect();

    // Build a JSON-encoded SQLite event (insert, update, delete).
    let event = {
      let json_value = serde_json::Value::Object(
        record
          .iter()
          .filter_map(|(name, value)| {
            if let Ok(v) = valueref_to_json(*value) {
              return Some(((*name).to_string(), v));
            };
            return None;
          })
          .collect(),
      );

      let db_event = match action {
        RecordAction::Delete => DbEvent::Delete(Some(json_value)),
        RecordAction::Insert => DbEvent::Insert(Some(json_value)),
        RecordAction::Update => DbEvent::Update(Some(json_value)),
      };

      let Ok(event) = Event::default().json_data(db_event) else {
        return;
      };

      event
    };

    'record_subs: {
      let mut read_lock = s.record_subscriptions.upgradable_read();
      let Some(subs) = read_lock.get(table_name).and_then(|m| m.get(&rowid)) else {
        break 'record_subs;
      };

      let dead_subscriptions = Self::broker_subscriptions(s, conn, subs, &record, &event);
      if dead_subscriptions.is_empty() && action != RecordAction::Delete {
        // No cleanup needed.
        break 'record_subs;
      }

      read_lock.with_upgraded(move |subscriptions| {
        let Some(table_subscriptions) = subscriptions.get_mut(table_name) else {
          return;
        };

        if action == RecordAction::Delete {
          // Also drops the channel and thus automatically closes the SSE connection.
          table_subscriptions.remove(&rowid);

          if table_subscriptions.is_empty() {
            subscriptions.remove(table_name);
            if subscriptions.is_empty() && s.table_subscriptions.read().is_empty() {
              conn.preupdate_hook(NO_HOOK);
            }
          }

          return;
        }

        if let Some(m) = table_subscriptions.get_mut(&rowid) {
          for idx in dead_subscriptions.iter().rev() {
            m.swap_remove(*idx);
          }

          if m.is_empty() {
            table_subscriptions.remove(&rowid);

            if table_subscriptions.is_empty() {
              subscriptions.remove(table_name);
              if subscriptions.is_empty() && s.table_subscriptions.read().is_empty() {
                conn.preupdate_hook(NO_HOOK);
              }
            }
          }
        }
      });
    }

    'table_subs: {
      let mut read_lock = s.table_subscriptions.upgradable_read();
      let Some(subs) = read_lock.get(table_name) else {
        break 'table_subs;
      };

      let dead_subscriptions = Self::broker_subscriptions(s, conn, subs, &record, &event);
      if dead_subscriptions.is_empty() && action != RecordAction::Delete {
        // No cleanup needed.
        break 'table_subs;
      }

      read_lock.with_upgraded(move |subscriptions| {
        let Some(table_subscriptions) = subscriptions.get_mut(table_name) else {
          return;
        };

        for idx in dead_subscriptions.iter().rev() {
          table_subscriptions.swap_remove(*idx);
        }

        if table_subscriptions.is_empty() {
          subscriptions.remove(table_name);

          if subscriptions.is_empty() && s.record_subscriptions.read().is_empty() {
            conn.preupdate_hook(NO_HOOK);
          }
        }
      });
    }
  }

  async fn add_hook(&self) -> trailbase_sqlite::connection::Result<()> {
    let state = &self.state;
    let conn = state.conn.clone();
    let s = state.clone();

    return state
      .conn
      .add_preupdate_hook(Some(
        move |action: Action, db: &str, table_name: &str, case: &PreUpdateCase| {
          assert_eq!(db, "main");

          let action: RecordAction = match action {
            Action::SQLITE_UPDATE | Action::SQLITE_INSERT | Action::SQLITE_DELETE => action.into(),
            a => {
              log::error!("Unknown action: {a:?}");
              return;
            }
          };

          let Some(rowid) = extract_row_id(case) else {
            log::error!("Failed to extract row id");
            return;
          };

          // If there are no subscriptions, do nothing.
          let record_subs_candidate = s
            .record_subscriptions
            .read()
            .get(table_name)
            .and_then(|m| m.get(&rowid))
            .is_some();
          let table_subs_candidate = s.table_subscriptions.read().get(table_name).is_some();
          if !record_subs_candidate && !table_subs_candidate {
            return;
          }

          let Some(record_values) = extract_record_values(case) else {
            log::error!("Failed to extract values");
            return;
          };

          let state = ContinuationState {
            state: s.clone(),
            table_metadata: s.table_metadata.get(table_name),
            action,
            table_name: table_name.to_string(),
            rowid,
            record_values,
          };

          // TODO: Optimization: in cases where there's only table-level access restrictions, we
          // could avoid the continuation and even dispatch the subscription handling to a
          // different thread entirely to take more work off the SQLite thread.
          conn.call_and_forget(move |conn| {
            Self::hook_continuation(conn, state);
          });
        },
      ))
      .await;
  }

  async fn add_record_subscription(
    &self,
    api: RecordApi,
    record: trailbase_sqlite::Value,
    user: Option<User>,
  ) -> Result<(async_channel::Receiver<Event>, i64, i64), RecordError> {
    let table_name = api.table_name();
    let pk_column = &api.record_pk_column().name;

    let Some(row) = self
      .state
      .conn
      .query_row(
        &format!(r#"SELECT _rowid_ FROM "{table_name}" WHERE "{pk_column}" = $1"#),
        [record],
      )
      .await?
    else {
      return Err(RecordError::RecordNotFound);
    };
    let row_id: i64 = row
      .get(0)
      .map_err(|err| RecordError::Internal(err.into()))?;

    let (sender, receiver) = async_channel::bounded::<Event>(16);

    let subscription_id = SUBSCRIPTION_COUNTER.fetch_add(1, Ordering::SeqCst);
    let empty = {
      let mut lock = self.state.record_subscriptions.write();
      let empty = lock.is_empty();
      let m: &mut HashMap<i64, Vec<Subscription>> = lock.entry(table_name.to_string()).or_default();

      m.entry(row_id).or_default().push(Subscription {
        subscription_id,
        record_api_name: api.api_name().to_string(),
        // record_id: Some(record),
        user,
        channel: sender,
      });

      empty
    };

    if empty {
      self.add_hook().await.unwrap();
    }

    return Ok((receiver, subscription_id, row_id));
  }

  async fn add_table_subscription(
    &self,
    api: RecordApi,
    user: Option<User>,
  ) -> Result<(async_channel::Receiver<Event>, i64), RecordError> {
    let state = &self.state;
    let table_name = api.table_name();

    let (sender, receiver) = async_channel::bounded::<Event>(16);
    let subscription_id = SUBSCRIPTION_COUNTER.fetch_add(1, Ordering::SeqCst);
    let empty = {
      let mut lock = state.table_subscriptions.write();
      let empty = lock.is_empty() && state.record_subscriptions.read().is_empty();
      let m: &mut Vec<Subscription> = lock.entry(table_name.to_string()).or_default();

      m.push(Subscription {
        subscription_id,
        record_api_name: api.api_name().to_string(),
        user,
        channel: sender,
      });

      empty
    };

    if empty {
      self.add_hook().await.unwrap();
    }

    return Ok((receiver, subscription_id));
  }

  // TODO: Cleaning up subscriptions might be a thing, e.g. if SSE handlers had an onDisconnect
  // handler. Right now we're handling cleanups reactively, i.e. we only remove subscriptions when
  // sending new events and the receiving end of a handler channel became invalid. It would
  // be better to be pro-active and remove subscriptions sooner.
  //
  // NOTE: To implement progressive cleanup, we should probably just tie an RAII object to the
  // Sse::new(receiver.map(<closure>)).
  //
  // async fn cleanup_subscription(&self, subscription_id: SubscriptionId) -> Result<(),
  // RecordError> {   let mut lock = self.state.subscriptions.write();
  //
  //   if let Some(table_subs) = lock.get_mut(&subscription_id.table_name) {
  //     if let Some(subs) = table_subs.get_mut(&subscription_id.row_id) {
  //       subs.retain(|s| s.id != subscription_id.subscription_id);
  //
  //       if subs.is_empty() {
  //         table_subs.remove(&subscription_id.row_id);
  //       }
  //     }
  //
  //     if table_subs.is_empty() {
  //       lock.remove(&subscription_id.table_name);
  //     }
  //   }
  //
  //   if lock.is_empty() {
  //     Self::remove_preupdate_hook(&*self.state).await?;
  //   }
  //
  //   return Ok(());
  // }
}

pub async fn add_subscription_sse_handler(
  State(state): State<AppState>,
  Path((api_name, record)): Path<(String, String)>,
  user: Option<User>,
) -> Result<Response, RecordError> {
  let Some(api) = state.lookup_record_api(&api_name) else {
    return Err(RecordError::ApiNotFound);
  };

  if record == "*" {
    api.check_table_level_access(Permission::Read, user.as_ref())?;

    let mgr = state.subscription_manager();
    let (receiver, _sub_id) = mgr.add_table_subscription(api, user).await?;

    let encode = move |ev: Event| -> Result<Event, axum::Error> {
      return Ok(ev);
    };

    return Ok(
      Sse::new(receiver.map(encode))
        .keep_alive(KeepAlive::default())
        .into_response(),
    );
  } else {
    let record_id = api.id_to_sql(&record)?;
    api
      .check_record_level_access(Permission::Read, Some(&record_id), None, user.as_ref())
      .await?;

    let mgr = state.subscription_manager();
    let table_name = api.table_name().to_string();
    let (receiver, sub_id, row_id) = mgr.add_record_subscription(api, record_id, user).await?;

    let mgr_state = mgr.state.clone();
    let guard = scopeguard::guard((), move |_| {
      state.conn().call_and_forget(move |conn| {
        let mut lock = mgr_state.record_subscriptions.write();
        let subs = lock.get_mut(&table_name).and_then(|x| x.get_mut(&row_id));
        if let Some(subs) = subs {
          subs.retain(|sub| {
            return sub.subscription_id != sub_id;
          });

          if subs.is_empty() {
            let table = lock.get_mut(&table_name).unwrap();
            table.remove(&row_id);

            if table.is_empty() {
              lock.remove(&table_name);

              if lock.is_empty() && mgr_state.table_subscriptions.read().is_empty() {
                conn.preupdate_hook(NO_HOOK);
              }
            }
          }
        }
      });
    });

    let encode = move |ev: Event| -> Result<Event, axum::Error> {
      let _x = &guard;
      return Ok(ev);
    };

    return Ok(
      Sse::new(receiver.map(encode))
        .keep_alive(KeepAlive::default())
        .into_response(),
    );
  }
}

#[cfg(test)]
async fn decode_sse_json_event(event: Event) -> serde_json::Value {
  use axum::response::IntoResponse;

  let (sender, receiver) = async_channel::unbounded::<Event>();
  let sse = Sse::new(receiver.map(|ev| -> Result<Event, axum::Error> { Ok(ev) }));

  sender.send(event).await.unwrap();
  sender.close();

  let resp = sse.into_response();
  let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
    .await
    .unwrap();

  let str = String::from_utf8_lossy(&bytes);
  let x = str
    .strip_prefix("data: ")
    .unwrap()
    .strip_suffix("\n\n")
    .unwrap();
  return serde_json::from_str(x).unwrap();
}

#[cfg(test)]
mod tests {
  use super::DbEvent;
  use super::*;
  use trailbase_sqlite::params;

  use crate::app_state::test_state;
  use crate::records::{add_record_api, AccessRules, Acls, PermissionFlag};

  async fn decode_db_event(event: Event) -> DbEvent {
    let json = decode_sse_json_event(event).await;
    return serde_json::from_value(json).unwrap();
  }

  #[tokio::test]
  async fn sse_event_encoding_test() {
    let json = serde_json::json!({
      "a": 5,
      "b": "text",
    });
    let db_event = DbEvent::Delete(Some(json));
    let event = Event::default().json_data(db_event.clone()).unwrap();

    assert_eq!(decode_db_event(event).await, db_event);
  }

  async fn setup() -> AppState {
    let state = test_state(None).await.unwrap();
    let conn = state.conn().clone();

    conn
      .execute(
        "CREATE TABLE test (id INTEGER PRIMARY KEY, text TEXT) STRICT",
        (),
      )
      .await
      .unwrap();

    state.table_metadata().invalidate_all().await.unwrap();

    // Register message table as record api with moderator read access.
    add_record_api(
      &state,
      "api_name",
      "test",
      Acls {
        world: vec![PermissionFlag::Create, PermissionFlag::Read],
        ..Default::default()
      },
      AccessRules::default(),
    )
    .await
    .unwrap();

    return state;
  }

  #[tokio::test]
  async fn subscribe_to_record_test() {
    let state = setup().await;
    let conn = state.conn().clone();

    let record_id_raw = 0;
    let record_id = trailbase_sqlite::Value::Integer(record_id_raw);
    let rowid: i64 = conn
      .query_row(
        "INSERT INTO test (id, text) VALUES ($1, 'foo') RETURNING _rowid_",
        [record_id],
      )
      .await
      .unwrap()
      .unwrap()
      .get(0)
      .unwrap();

    assert_eq!(rowid, record_id_raw);

    let manager = state.subscription_manager();
    let api = state.lookup_record_api("api_name").unwrap();
    let (receiver, _sub_id, _row_id) = manager
      .add_record_subscription(api, trailbase_sqlite::Value::Integer(0), None)
      .await
      .unwrap();

    assert_eq!(1, manager.num_record_subscriptions());

    conn
      .execute(
        "UPDATE test SET text = $1 WHERE _rowid_ = $2",
        params!("bar", rowid),
      )
      .await
      .unwrap();

    let expected = serde_json::json!({
      "id": record_id_raw,
      "text": "bar",
    });
    match decode_db_event(receiver.recv().await.unwrap()).await {
      DbEvent::Update(Some(value)) => {
        assert_eq!(value, expected);
      }
      x => {
        assert!(false, "Expected update, got: {x:?}");
      }
    };

    conn
      .execute("DELETE FROM test WHERE _rowid_ = $1", params!(rowid))
      .await
      .unwrap();

    match decode_db_event(receiver.recv().await.unwrap()).await {
      DbEvent::Delete(Some(value)) => {
        assert_eq!(value, expected);
      }
      x => {
        assert!(false, "Expected update, got: {x:?}");
      }
    }

    assert_eq!(0, manager.num_record_subscriptions());
  }

  #[tokio::test]
  async fn subscribe_to_table_test() {
    let state = setup().await;
    let conn = state.conn().clone();

    let manager = state.subscription_manager();
    let api = state.lookup_record_api("api_name").unwrap();
    let (receiver, _id) = manager.add_table_subscription(api, None).await.unwrap();

    let record_id_raw = 0;
    conn
      .query_row(
        "INSERT INTO test (id, text) VALUES ($1, 'foo')",
        params!(record_id_raw),
      )
      .await
      .unwrap();

    conn
      .execute(
        "UPDATE test SET text = $1 WHERE id = $2",
        params!("bar", record_id_raw),
      )
      .await
      .unwrap();

    let expected = serde_json::json!({
      "id": record_id_raw,
      "text": "foo",
    });
    match decode_db_event(receiver.recv().await.unwrap()).await {
      DbEvent::Insert(Some(value)) => {
        assert_eq!(value, expected);
      }
      x => {
        assert!(false, "Expected update, got: {x:?}");
      }
    };

    let expected = serde_json::json!({
      "id": record_id_raw,
      "text": "bar",
    });
    match decode_db_event(receiver.recv().await.unwrap()).await {
      DbEvent::Update(Some(value)) => {
        assert_eq!(value, expected);
      }
      x => {
        assert!(false, "Expected update, got: {x:?}");
      }
    };

    conn
      .execute("DELETE FROM test WHERE id = $1", params!(record_id_raw))
      .await
      .unwrap();

    match decode_db_event(receiver.recv().await.unwrap()).await {
      DbEvent::Delete(Some(value)) => {
        assert_eq!(value, expected);
      }
      x => {
        assert!(false, "Expected update, got: {x:?}");
      }
    }
  }

  #[tokio::test]
  async fn subscription_lifecycle_test() {
    let state = setup().await;
    let conn = state.conn().clone();

    let record_id_raw = 0;
    let record_id = trailbase_sqlite::Value::Integer(record_id_raw);
    let rowid: i64 = conn
      .query_row(
        "INSERT INTO test (id, text) VALUES ($1, 'foo') RETURNING _rowid_",
        [record_id],
      )
      .await
      .unwrap()
      .unwrap()
      .get(0)
      .unwrap();

    assert_eq!(rowid, record_id_raw);

    let sse = add_subscription_sse_handler(
      State(state.clone()),
      Path(("api_name".to_string(), record_id_raw.to_string())),
      None,
    )
    .await;

    let manager = state.subscription_manager();
    assert_eq!(1, manager.num_record_subscriptions());

    drop(sse);

    // Implicitly await for the cleanup to be scheduled on the sqlite executor.
    conn.query("SELECT 1", ()).await.unwrap();

    assert_eq!(0, manager.num_record_subscriptions());
  }
}

const NO_HOOK: Option<fn(Action, &str, &str, &PreUpdateCase)> = None;
