//! Client-owned gateway tokens, keyed by gateway origin.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub fn token_path() -> PathBuf {
    let root = std::env::var_os("SWAGE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".swage")
        });
    root.join("client-pairings.json")
}

pub(crate) fn origin(address: &str) -> io::Result<String> {
    let url = reqwest::Url::parse(address).map_err(io::Error::other)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(io::Error::other(
            "gateway address must be an HTTP origin without credentials",
        ));
    }
    Ok(url.origin().ascii_serialization())
}

fn read(path: &Path) -> io::Result<BTreeMap<String, String>> {
    match std::fs::read(path) {
        Ok(body) => serde_json::from_slice(&body).map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(error),
    }
}

pub(crate) fn load(path: &Path, address: &str) -> io::Result<Option<String>> {
    Ok(read(path)?.remove(&origin(address)?))
}

pub(crate) fn save(path: &Path, address: &str, token: &str) -> io::Result<()> {
    let mut tokens = read(path)?;
    tokens.insert(origin(address)?, token.to_owned());
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("token path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".pairings-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&serde_json::to_vec(&tokens).map_err(io::Error::other)?)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}
