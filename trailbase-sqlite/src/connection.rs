use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;
pub use rusqlite::types::{ToSqlOutput, Value};
use rusqlite::{types, Statement};
use std::{
  cell::{OnceCell, RefCell},
  fmt::{self, Debug},
  str::FromStr,
  sync::Arc,
};
use tokio::sync::oneshot;

use crate::error::Error;
pub use crate::params::Params;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[macro_export]
macro_rules! params {
    () => {
        [] as [$crate::params::ToSqlType]
    };
    ($($param:expr),+ $(,)?) => {
        [$(Into::<$crate::params::ToSqlType>::into($param)),+]
    };
}

#[macro_export]
macro_rules! named_params {
    () => {
        [] as [(&str, $crate::params::ToSqlType)]
    };
    ($($param_name:literal: $param_val:expr),+ $(,)?) => {
        [$(($param_name as &str, Into::<$crate::params::ToSqlType>::into($param_val))),+]
    };
}

/// The result returned on method calls in this crate.
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Copy, Clone)]
pub enum ValueType {
  Integer = 1,
  Real,
  Text,
  Blob,
  Null,
}

impl FromStr for ValueType {
  type Err = ();

  fn from_str(s: &str) -> std::result::Result<ValueType, Self::Err> {
    match s {
      "TEXT" => Ok(ValueType::Text),
      "INTEGER" => Ok(ValueType::Integer),
      "BLOB" => Ok(ValueType::Blob),
      "NULL" => Ok(ValueType::Null),
      "REAL" => Ok(ValueType::Real),
      _ => Err(()),
    }
  }
}

#[allow(unused)]
#[derive(Debug)]
pub struct Column {
  name: String,
  decl_type: Option<ValueType>,
}

#[derive(Debug)]
pub struct Rows(Vec<Row>, Arc<Vec<Column>>);

fn columns(stmt: &Statement<'_>) -> Vec<Column> {
  return stmt
    .columns()
    .into_iter()
    .map(|c| Column {
      name: c.name().to_string(),
      decl_type: c.decl_type().and_then(|s| ValueType::from_str(s).ok()),
    })
    .collect();
}

impl Rows {
  pub fn from_rows(mut rows: rusqlite::Rows) -> rusqlite::Result<Self> {
    let columns: Arc<Vec<Column>> = Arc::new(rows.as_ref().map_or(vec![], columns));

    let mut result = vec![];
    while let Some(row) = rows.next()? {
      result.push(Row::from_row(row, Some(columns.clone()))?);
    }

    Ok(Self(result, columns))
  }

  #[cfg(test)]
  pub fn len(&self) -> usize {
    self.0.len()
  }

  pub fn iter(&self) -> std::slice::Iter<'_, Row> {
    self.0.iter()
  }

  pub fn column_count(&self) -> usize {
    self.1.len()
  }

  pub fn column_names(&self) -> Vec<&str> {
    self.1.iter().map(|s| s.name.as_str()).collect()
  }

  pub fn column_name(&self, idx: usize) -> Option<&str> {
    self.1.get(idx).map(|c| c.name.as_str())
  }

  pub fn column_type(&self, idx: usize) -> std::result::Result<ValueType, rusqlite::Error> {
    if let Some(c) = self.1.get(idx) {
      return c.decl_type.ok_or_else(|| {
        rusqlite::Error::InvalidColumnType(
          idx,
          self.column_name(idx).unwrap_or("?").to_string(),
          types::Type::Null,
        )
      });
    }
    return Err(rusqlite::Error::InvalidColumnType(
      idx,
      self.column_name(idx).unwrap_or("?").to_string(),
      types::Type::Null,
    ));
  }
}

#[derive(Debug)]
pub struct Row(Vec<types::Value>, Arc<Vec<Column>>);

impl Row {
  pub fn from_row(row: &rusqlite::Row, cols: Option<Arc<Vec<Column>>>) -> rusqlite::Result<Self> {
    let columns = cols.unwrap_or_else(|| Arc::new(columns(row.as_ref())));

    let count = columns.len();
    let mut values = Vec::<types::Value>::with_capacity(count);
    for idx in 0..count {
      values.push(row.get_ref(idx)?.into());
    }

    Ok(Self(values, columns))
  }

  pub fn get<T>(&self, idx: usize) -> types::FromSqlResult<T>
  where
    T: types::FromSql,
  {
    let val = self
      .0
      .get(idx)
      .ok_or_else(|| types::FromSqlError::Other("Index out of bounds".into()))?;
    T::column_result(val.into())
  }

  pub fn get_value(&self, idx: usize) -> Result<types::Value> {
    self
      .0
      .get(idx)
      .ok_or_else(|| Error::Other("Index out of bounds".into()))
      .cloned()
  }

  pub fn column_count(&self) -> usize {
    self.0.len()
  }

  pub fn column_names(&self) -> Vec<&str> {
    self.1.iter().map(|s| s.name.as_str()).collect()
  }

  pub fn column_name(&self, idx: usize) -> Option<&str> {
    self.1.get(idx).map(|c| c.name.as_str())
  }
}

type CallFn = Box<dyn FnOnce(&mut rusqlite::Connection) + Send + 'static>;

enum Message {
  Run(CallFn),
  Close(oneshot::Sender<std::result::Result<(), rusqlite::Error>>),
}

const MAX_ID: usize = 4;
static ID: Mutex<usize> = Mutex::new(0);
thread_local! {
  static CELLS : Vec<OnceCell<RefCell<rusqlite::Connection>>> = std::iter::repeat_with(OnceCell::new).take(MAX_ID).collect();
}

trait ConnTrait {
  fn call<T, F>(&self, f: F) -> Result<T>
  where
    F: FnOnce(&mut rusqlite::Connection) -> Result<T>;
}

struct ThreadLocalConn {
  id: usize,
  fun: Box<dyn Fn() -> rusqlite::Connection + Send + Sync>,
}

#[allow(unused)]
impl ThreadLocalConn {
  pub fn new(f: impl Fn() -> rusqlite::Connection + Send + Sync + 'static) -> Self {
    let id = {
      let mut lock = ID.lock();
      let id = *lock;
      *lock += 1;
      id
    };
    if id >= MAX_ID {
      panic!("");
    }
    return Self {
      id,
      fun: Box::new(f),
    };
  }
}

impl ConnTrait for ThreadLocalConn {
  #[inline]
  fn call<T, F>(&self, f: F) -> Result<T>
  where
    F: FnOnce(&mut rusqlite::Connection) -> Result<T>,
  {
    return CELLS.with(|cells| {
      let c = cells[self.id].get_or_init(|| {
        let new_conn = (self.fun)();
        // HACKY: overriding busy handling.
        new_conn
          .busy_timeout(std::time::Duration::from_secs(10))
          .unwrap();
        new_conn
          .busy_handler(Some(|_attempts| {
            std::thread::sleep(std::time::Duration::from_millis(10));
            return true;
          }))
          .unwrap();

        RefCell::new(new_conn)
      });
      let conn: &mut rusqlite::Connection = &mut c.borrow_mut();
      return f(conn);
    });
  }
}

#[allow(unused)]
struct SharedConn {
  conn: Mutex<rusqlite::Connection>,
}

impl SharedConn {
  pub fn new(f: impl Fn() -> rusqlite::Connection + Send + Sync + 'static) -> Self {
    return Self {
      conn: Mutex::new(f()),
    };
  }
}

impl ConnTrait for SharedConn {
  #[inline]
  fn call<T, F>(&self, f: F) -> Result<T>
  where
    F: FnOnce(&mut rusqlite::Connection) -> Result<T>,
  {
    let mut lock = self.conn.lock();
    return f(&mut lock);
  }
}

/// A handle to call functions in background thread.
#[derive(Clone)]
pub struct Connection {
  // sender: Sender<Message>,
  #[cfg(not(debug_assertions))]
  conn: Arc<ThreadLocalConn>,
  #[cfg(debug_assertions)]
  conn: Arc<SharedConn>,
}

impl Connection {
  pub async fn from_conn(
    f: impl Fn() -> rusqlite::Connection + Send + Sync + 'static,
  ) -> Result<Self> {
    return Ok(Connection {
      #[cfg(not(debug_assertions))]
      conn: Arc::new(ThreadLocalConn::new(f)),
      #[cfg(debug_assertions)]
      conn: Arc::new(SharedConn::new(f)),
    });
    // return Ok(start(move || Ok(f())).await?);
  }

  /// Open a new connection to an in-memory SQLite database.
  ///
  /// # Failure
  ///
  /// Will return `Err` if the underlying SQLite open call fails.
  pub async fn open_in_memory() -> Result<Self> {
    return Ok(Connection {
      #[cfg(not(debug_assertions))]
      conn: Arc::new(ThreadLocalConn::new(|| {
        rusqlite::Connection::open_in_memory().unwrap()
      })),
      #[cfg(debug_assertions)]
      conn: Arc::new(SharedConn::new(|| {
        rusqlite::Connection::open_in_memory().unwrap()
      })),
    });

    // return Ok(start(rusqlite::Connection::open_in_memory).await?);
  }

  // /// Call a function in background thread and get the result
  // /// asynchronously.
  // ///
  // /// # Failure
  // ///
  // /// Will return `Err` if the database connection has been closed.
  // pub async fn call<F, R>(&self, function: F) -> Result<R>
  // where
  //   F: FnOnce(&mut rusqlite::Connection) -> Result<R> + 'static + Send,
  //   R: Send + 'static,
  // {
  //   let (sender, receiver) = oneshot::channel::<Result<R>>();
  //
  //   self
  //     .sender
  //     .send(Message::Run(Box::new(move |conn| {
  //       let value = function(conn);
  //       let _ = sender.send(value);
  //     })))
  //     .map_err(|_| Error::ConnectionClosed)?;
  //
  //   receiver.await.map_err(|_| Error::ConnectionClosed)?
  // }

  pub async fn call<F, R>(&self, function: F) -> Result<R>
  where
    F: FnOnce(&mut rusqlite::Connection) -> Result<R> + 'static + Send,
    R: Send + 'static,
  {
    return self.conn.call(function);
  }

  /// Query SQL statement.
  pub async fn query(&self, sql: &str, params: impl Params + Send + 'static) -> Result<Rows> {
    let sql = sql.to_string();
    return self.conn.call(move |conn: &mut rusqlite::Connection| {
      let mut stmt = conn.prepare(&sql)?;
      params.bind(&mut stmt)?;
      let rows = stmt.raw_query();
      Ok(Rows::from_rows(rows)?)
    });
  }

  pub async fn query_row(
    &self,
    sql: &str,
    params: impl Params + Send + 'static,
  ) -> Result<Option<Row>> {
    let sql = sql.to_string();
    return self.conn.call(move |conn: &mut rusqlite::Connection| {
      let mut stmt = conn.prepare(&sql)?;
      params.bind(&mut stmt)?;
      let mut rows = stmt.raw_query();
      if let Some(row) = rows.next()? {
        return Ok(Some(Row::from_row(row, None)?));
      }
      Ok(None)
    });
  }

  pub async fn query_value<T: serde::de::DeserializeOwned + Send + 'static>(
    &self,
    sql: &str,
    params: impl Params + Send + 'static,
  ) -> Result<Option<T>> {
    let sql = sql.to_string();
    return self.conn.call(move |conn: &mut rusqlite::Connection| {
      let mut stmt = conn.prepare(&sql)?;
      params.bind(&mut stmt)?;
      let mut rows = stmt.raw_query();
      if let Some(row) = rows.next()? {
        return Ok(Some(serde_rusqlite::from_row(row)?));
      }
      Ok(None)
    });
  }

  pub async fn query_values<T: serde::de::DeserializeOwned + Send + 'static>(
    &self,
    sql: &str,
    params: impl Params + Send + 'static,
  ) -> Result<Vec<T>> {
    let sql = sql.to_string();
    return self.conn.call(move |conn: &mut rusqlite::Connection| {
      let mut stmt = conn.prepare(&sql)?;
      params.bind(&mut stmt)?;
      let mut rows = stmt.raw_query();

      let mut values = vec![];
      while let Some(row) = rows.next()? {
        values.push(serde_rusqlite::from_row(row)?);
      }
      return Ok(values);
    });
  }

  /// Execute SQL statement.
  pub async fn execute(&self, sql: &str, params: impl Params + Send + 'static) -> Result<usize> {
    let sql = sql.to_string();
    return self.conn.call(move |conn: &mut rusqlite::Connection| {
      let mut stmt = conn.prepare(&sql)?;
      params.bind(&mut stmt)?;
      Ok(stmt.raw_execute()?)
    });
  }

  /// Batch execute SQL statements and return rows of last statement.
  pub async fn execute_batch(&self, sql: &str) -> Result<Option<Rows>> {
    let sql = sql.to_string();
    return self.conn.call(move |conn: &mut rusqlite::Connection| {
      let batch = rusqlite::Batch::new(conn, &sql);

      let mut p = batch.peekable();
      while let Some(iter) = p.next() {
        let mut stmt = iter?;

        let mut rows = stmt.raw_query();
        let row = rows.next()?;
        if p.peek().is_none() {
          if let Some(row) = row {
            let cols: Arc<Vec<Column>> = Arc::new(columns(row.as_ref()));

            let mut result = vec![Row::from_row(row, Some(cols.clone()))?];
            while let Some(row) = rows.next()? {
              result.push(Row::from_row(row, Some(cols.clone()))?);
            }
            return Ok(Some(Rows(result, cols)));
          }
          return Ok(None);
        }
      }
      return Ok(None);
    });
  }

  /// Close the database connection.
  ///
  /// This is functionally equivalent to the `Drop` implementation for
  /// `Connection`. It consumes the `Connection`, but on error returns it
  /// to the caller for retry purposes.
  ///
  /// If successful, any following `close` operations performed
  /// on `Connection` copies will succeed immediately.
  ///
  /// On the other hand, any calls to [`Connection::call`] will return a
  /// [`Error::ConnectionClosed`], and any calls to [`Connection::call_unwrap`] will cause a
  /// `panic`.
  ///
  /// # Failure
  ///
  /// Will return `Err` if the underlying SQLite close call fails.
  pub async fn close(self) -> Result<()> {
    return Ok(());

    // let (sender, receiver) = oneshot::channel::<std::result::Result<(), rusqlite::Error>>();
    //
    // if let Err(crossbeam_channel::SendError(_)) = self.sender.send(Message::Close(sender)) {
    //   // If the channel is closed on the other side, it means the connection closed successfully
    //   // This is a safeguard against calling close on a `Copy` of the connection
    //   return Ok(());
    // }
    //
    // let result = receiver.await;
    //
    // if result.is_err() {
    //   // If we get a RecvError at this point, it also means the channel closed in the meantime
    //   // we can assume the connection is closed
    //   return Ok(());
    // }
    //
    // result.unwrap().map_err(|e| Error::Close(self, e))
  }
}

impl Debug for Connection {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Connection").finish()
  }
}

/// A handle to call functions in background thread.
#[derive(Clone)]
pub struct AsyncConnection {
  sender: Sender<Message>,
}

impl AsyncConnection {
  pub async fn from_conn(conn: rusqlite::Connection) -> Result<Self> {
    return Ok(start(move || Ok(conn)).await?);
  }

  /// Open a new connection to an in-memory SQLite database.
  ///
  /// # Failure
  ///
  /// Will return `Err` if the underlying SQLite open call fails.
  pub async fn open_in_memory() -> Result<Self> {
    return Ok(start(rusqlite::Connection::open_in_memory).await?);
  }

  /// Call a function in background thread and get the result
  /// asynchronously.
  ///
  /// # Failure
  ///
  /// Will return `Err` if the database connection has been closed.
  pub async fn call<F, R>(&self, function: F) -> Result<R>
  where
    F: FnOnce(&mut rusqlite::Connection) -> Result<R> + 'static + Send,
    R: Send + 'static,
  {
    let (sender, receiver) = oneshot::channel::<Result<R>>();

    self
      .sender
      .send(Message::Run(Box::new(move |conn| {
        let value = function(conn);
        let _ = sender.send(value);
      })))
      .map_err(|_| Error::ConnectionClosed)?;

    receiver.await.map_err(|_| Error::ConnectionClosed)?
  }

  /// Query SQL statement.
  pub async fn query(&self, sql: &str, params: impl Params + Send + 'static) -> Result<Rows> {
    let sql = sql.to_string();
    return self
      .call(move |conn: &mut rusqlite::Connection| {
        let mut stmt = conn.prepare(&sql)?;
        params.bind(&mut stmt)?;
        let rows = stmt.raw_query();
        Ok(Rows::from_rows(rows)?)
      })
      .await;
  }

  pub async fn query_row(
    &self,
    sql: &str,
    params: impl Params + Send + 'static,
  ) -> Result<Option<Row>> {
    let sql = sql.to_string();
    return self
      .call(move |conn: &mut rusqlite::Connection| {
        let mut stmt = conn.prepare(&sql)?;
        params.bind(&mut stmt)?;
        let mut rows = stmt.raw_query();
        if let Some(row) = rows.next()? {
          return Ok(Some(Row::from_row(row, None)?));
        }
        Ok(None)
      })
      .await;
  }

  pub async fn query_value<T: serde::de::DeserializeOwned + Send + 'static>(
    &self,
    sql: &str,
    params: impl Params + Send + 'static,
  ) -> Result<Option<T>> {
    let sql = sql.to_string();
    return self
      .call(move |conn: &mut rusqlite::Connection| {
        let mut stmt = conn.prepare(&sql)?;
        params.bind(&mut stmt)?;
        let mut rows = stmt.raw_query();
        if let Some(row) = rows.next()? {
          return Ok(Some(serde_rusqlite::from_row(row)?));
        }
        Ok(None)
      })
      .await;
  }

  pub async fn query_values<T: serde::de::DeserializeOwned + Send + 'static>(
    &self,
    sql: &str,
    params: impl Params + Send + 'static,
  ) -> Result<Vec<T>> {
    let sql = sql.to_string();
    return self
      .call(move |conn: &mut rusqlite::Connection| {
        let mut stmt = conn.prepare(&sql)?;
        params.bind(&mut stmt)?;
        let mut rows = stmt.raw_query();

        let mut values = vec![];
        while let Some(row) = rows.next()? {
          values.push(serde_rusqlite::from_row(row)?);
        }
        return Ok(values);
      })
      .await;
  }

  /// Execute SQL statement.
  pub async fn execute(&self, sql: &str, params: impl Params + Send + 'static) -> Result<usize> {
    let sql = sql.to_string();
    return self
      .call(move |conn: &mut rusqlite::Connection| {
        let mut stmt = conn.prepare(&sql)?;
        params.bind(&mut stmt)?;
        Ok(stmt.raw_execute()?)
      })
      .await;
  }

  /// Batch execute SQL statements and return rows of last statement.
  pub async fn execute_batch(&self, sql: &str) -> Result<Option<Rows>> {
    let sql = sql.to_string();
    return self
      .call(move |conn: &mut rusqlite::Connection| {
        let batch = rusqlite::Batch::new(conn, &sql);

        let mut p = batch.peekable();
        while let Some(iter) = p.next() {
          let mut stmt = iter?;

          let mut rows = stmt.raw_query();
          let row = rows.next()?;
          if p.peek().is_none() {
            if let Some(row) = row {
              let cols: Arc<Vec<Column>> = Arc::new(columns(row.as_ref()));

              let mut result = vec![Row::from_row(row, Some(cols.clone()))?];
              while let Some(row) = rows.next()? {
                result.push(Row::from_row(row, Some(cols.clone()))?);
              }
              return Ok(Some(Rows(result, cols)));
            }
            return Ok(None);
          }
        }
        return Ok(None);
      })
      .await;
  }

  /// Close the database connection.
  ///
  /// This is functionally equivalent to the `Drop` implementation for
  /// `Connection`. It consumes the `Connection`, but on error returns it
  /// to the caller for retry purposes.
  ///
  /// If successful, any following `close` operations performed
  /// on `Connection` copies will succeed immediately.
  ///
  /// On the other hand, any calls to [`Connection::call`] will return a
  /// [`Error::ConnectionClosed`], and any calls to [`Connection::call_unwrap`] will cause a
  /// `panic`.
  ///
  /// # Failure
  ///
  /// Will return `Err` if the underlying SQLite close call fails.
  pub async fn close(self) -> Result<()> {
    let (sender, receiver) = oneshot::channel::<std::result::Result<(), rusqlite::Error>>();

    if let Err(crossbeam_channel::SendError(_)) = self.sender.send(Message::Close(sender)) {
      // If the channel is closed on the other side, it means the connection closed successfully
      // This is a safeguard against calling close on a `Copy` of the connection
      return Ok(());
    }

    let result = receiver.await;

    if result.is_err() {
      // If we get a RecvError at this point, it also means the channel closed in the meantime
      // we can assume the connection is closed
      return Ok(());
    }

    result.unwrap().map_err(|e| Error::Close(self, e))
  }
}

impl Debug for AsyncConnection {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("AsyncConnection").finish()
  }
}

async fn start<F>(open: F) -> rusqlite::Result<AsyncConnection>
where
  F: FnOnce() -> rusqlite::Result<rusqlite::Connection> + Send + 'static,
{
  let (sender, receiver) = crossbeam_channel::unbounded::<Message>();
  let (result_sender, result_receiver) = oneshot::channel();

  std::thread::spawn(move || {
    let conn = match open() {
      Ok(c) => c,
      Err(e) => {
        let _ = result_sender.send(Err(e));
        return;
      }
    };

    if let Err(_e) = result_sender.send(Ok(())) {
      return;
    }

    event_loop(conn, receiver);
  });

  result_receiver
    .await
    .expect(BUG_TEXT)
    .map(|_| AsyncConnection { sender })
}

fn event_loop(mut conn: rusqlite::Connection, receiver: Receiver<Message>) {
  while let Ok(message) = receiver.recv() {
    match message {
      Message::Run(f) => f(&mut conn),
      Message::Close(s) => {
        match conn.close() {
          Ok(v) => s.send(Ok(v)).expect(BUG_TEXT),
          Err((_conn, e)) => s.send(Err(e)).expect(BUG_TEXT),
        };

        return;
      }
    }
  }
}

const BUG_TEXT: &str = "bug in trailbase-sqlite, please report";
