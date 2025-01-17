use serde::{Deserialize, Serialize};
use serde_json::json;
use trailbase_client::Client;

struct Server {
  child: std::process::Child,
}

impl Drop for Server {
  fn drop(&mut self) {
    self.child.kill().unwrap();
  }
}

const PORT: u16 = 4057;

fn start_server() -> Result<Server, std::io::Error> {
  let cwd = std::env::current_dir()?;
  assert!(cwd.ends_with("trailbase-rs"));

  let command_cwd = cwd.parent().unwrap().parent().unwrap();
  let depot_path = "client/testfixture";

  let _output = std::process::Command::new("cargo")
    .args(&["build"])
    .current_dir(&command_cwd)
    .output()?;

  let args = [
    "run".to_string(),
    "--".to_string(),
    format!("--data-dir={depot_path}"),
    "run".to_string(),
    format!("--address=127.0.0.1:{PORT}"),
    "--js-runtime-threads=2".to_string(),
  ];
  let child = std::process::Command::new("cargo")
    .args(&args)
    .current_dir(&command_cwd)
    .spawn()?;

  // Wait for server to become healthy.
  let runtime = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .unwrap();

  runtime.block_on(async {
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{PORT}/api/healthcheck");

    for _ in 0..100 {
      let response = client.get(&url).send().await;

      if let Ok(response) = response {
        if let Ok(body) = response.text().await {
          if body.to_uppercase() == "OK" {
            return;
          }
        }
      }

      tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    panic!("Server did not get healthy");
  });

  return Ok(Server { child });
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct SimpleStrict {
  id: String,

  text_null: Option<String>,
  text_default: Option<String>,
  text_not_null: String,
}

async fn connect() -> Client {
  let client = Client::new(&format!("http://127.0.0.1:{PORT}"), None);
  let _ = client.login("admin@localhost", "secret").await.unwrap();
  return client;
}

async fn login_test() {
  let client = connect().await;

  let tokens = client.tokens().unwrap();

  assert_ne!(tokens.auth_token, "");
  assert!(tokens.refresh_token.is_some());

  let user = client.user().unwrap();
  assert_eq!(user.email, "admin@localhost");

  client.refresh().await.unwrap();

  client.logout().await.unwrap();
  assert!(client.tokens().is_none());
}

async fn records_test() {
  let client = connect().await;
  let api = client.records("simple_strict_table");

  let now = now();

  let messages = vec![
    format!("rust client test 0: =?&{now}"),
    format!("rust client test 1: =?&{now}"),
  ];

  let mut ids = vec![];
  for msg in messages.iter() {
    ids.push(api.create(json!({"text_not_null": msg})).await.unwrap());
  }

  {
    // List one specific message.
    let filter = format!("text_not_null={}", messages[0]);
    let filters = vec![filter.as_str()];
    let records = api
      .list::<serde_json::Value>(None, None, Some(filters.as_slice()))
      .await
      .unwrap();

    assert_eq!(records.len(), 1);
  }

  {
    // List all the messages
    let filter = format!("text_not_null[like]=% =?&{now}");
    let records_ascending: Vec<SimpleStrict> = api
      .list(None, Some(&["+text_not_null"]), Some(&[&filter]))
      .await
      .unwrap();

    let messages_ascending: Vec<_> = records_ascending
      .into_iter()
      .map(|s| s.text_not_null)
      .collect();
    assert_eq!(messages, messages_ascending);

    let records_descending: Vec<SimpleStrict> = api
      .list(None, Some(&["-text_not_null"]), Some(&[&filter]))
      .await
      .unwrap();

    let messages_descending: Vec<_> = records_descending
      .into_iter()
      .map(|s| s.text_not_null)
      .collect();
    assert_eq!(
      messages,
      messages_descending.into_iter().rev().collect::<Vec<_>>()
    );
  }

  {
    // Read
    let record: SimpleStrict = api.read(&ids[0]).await.unwrap();
    assert_eq!(ids[0], record.id);
    assert_eq!(record.text_not_null, messages[0]);
  }

  {
    // Update
    let updated_message = format!("rust client updated test 0: {now}");
    api
      .update(&ids[0], json!({"text_not_null": updated_message}))
      .await
      .unwrap();

    let record: SimpleStrict = api.read(&ids[0]).await.unwrap();
    assert_eq!(record.text_not_null, updated_message);
  }

  {
    // Delete
    api.delete(&ids[0]).await.unwrap();

    let response = api.read::<SimpleStrict>(&ids[0]).await;
    assert!(response.is_err());
  }
}

#[test]
fn integration_test() {
  let _server = start_server().unwrap();

  let runtime = tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()
    .unwrap();

  runtime.block_on(login_test());
  println!("Ran login tests");

  runtime.block_on(records_test());
  println!("Ran records tests");
}

fn now() -> u64 {
  return std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .expect("Duration since epoch")
    .as_secs();
}
