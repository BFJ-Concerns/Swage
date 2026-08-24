use std::ffi::{OsStr, OsString};
use std::sync::{Mutex, MutexGuard};

use axum::http::StatusCode;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde_json::Value;
use swage_storage::MANAGED_SECRET_KEY_ENV;

static PROCESS_ENV_LOCK: Mutex<()> = Mutex::new(());

pub async fn live_http_json(
    address: std::net::SocketAddr,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let client = reqwest::Client::new();
    let mut request = client.request(method, format!("http://{address}{path}"));
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.expect("live gateway request");
    let status = response.status();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

pub struct ProcessEnvGuard {
    _lock: MutexGuard<'static, ()>,
    saved_vars: Vec<(OsString, Option<OsString>)>,
}

impl ProcessEnvGuard {
    pub fn new() -> Self {
        Self {
            _lock: PROCESS_ENV_LOCK
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
            saved_vars: Vec::new(),
        }
    }

    pub fn set_swage_home(&mut self, path: &std::path::Path) {
        self.set_var("SWAGE_HOME", path);
    }

    pub fn set_managed_secret_key(&mut self, byte: u8) {
        self.set_var(MANAGED_SECRET_KEY_ENV, BASE64.encode([byte; 32]));
    }

    pub fn remove_managed_secret_key(&mut self) {
        self.remove_var(MANAGED_SECRET_KEY_ENV);
    }

    pub fn set_var<K, V>(&mut self, key: K, value: V)
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.save_original(&key);
        unsafe { std::env::set_var(key, value) };
    }

    pub fn remove_var<K>(&mut self, key: K)
    where
        K: AsRef<OsStr>,
    {
        self.save_original(&key);
        unsafe { std::env::remove_var(key) };
    }

    fn save_original<K>(&mut self, key: K)
    where
        K: AsRef<OsStr>,
    {
        let key = key.as_ref().to_os_string();
        if self.saved_vars.iter().any(|(saved, _)| saved == &key) {
            return;
        }
        self.saved_vars.push((key.clone(), std::env::var_os(key)));
    }
}

impl Drop for ProcessEnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.saved_vars.iter().rev() {
            match value {
                Some(value) => unsafe { std::env::set_var(key, value) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }
}
