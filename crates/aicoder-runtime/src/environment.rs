use anyhow::Result;
use std::{collections::BTreeMap, path::PathBuf};
/// Hosts may supply a snapshot or refreshed lookup without mutating process environment.
pub trait Environment: Send + Sync {
    fn value(&self, name: &str) -> Result<Option<String>>;
    fn source(&self, name: &str) -> crate::ConfigSource {
        crate::ConfigSource::Environment {
            variable: name.into(),
        }
    }
    fn present(&self, name: &str) -> Result<bool> {
        Ok(self.value(name)?.is_some_and(|value| !value.is_empty()))
    }
    fn path(&self, name: &str) -> Result<Option<PathBuf>> {
        Ok(self.value(name)?.map(PathBuf::from))
    }
}
pub struct ProcessEnvironment;
impl Environment for ProcessEnvironment {
    fn present(&self, name: &str) -> Result<bool> {
        Ok(std::env::var_os(name).is_some_and(|value| !value.is_empty()))
    }
    fn value(&self, name: &str) -> Result<Option<String>> {
        match std::env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => anyhow::bail!("environment variable {name} is not Unicode"),
        }
    }
    fn path(&self, name: &str) -> Result<Option<PathBuf>> {
        Ok(std::env::var_os(name).map(PathBuf::from))
    }
}
impl Environment for BTreeMap<String, String> {
    fn value(&self, name: &str) -> Result<Option<String>> {
        Ok(self.get(name).cloned())
    }
}

/// Explicit dotenv input is a fallback source; it never modifies the process environment.
pub fn environment_file(path: Option<&std::path::Path>) -> Result<std::sync::Arc<dyn Environment>> {
    let process: std::sync::Arc<dyn Environment> = std::sync::Arc::new(ProcessEnvironment);
    let Some(path) = path else {
        return Ok(process);
    };
    let (path, bytes) = crate::files::read_bounded(path, crate::MAX_SETTINGS_BYTES, true)?
        .expect("required environment file");
    let mut values = BTreeMap::new();
    for item in dotenvy::from_read_iter(bytes.as_slice()) {
        let (key, value) =
            item.map_err(|_| anyhow::anyhow!("Invalid environment file {}", path.display()))?;
        anyhow::ensure!(
            !values.contains_key(&key),
            "Duplicate variable in environment file {}",
            path.display()
        );
        values.insert(key, value);
    }
    Ok(std::sync::Arc::new(FileEnvironment {
        process,
        path,
        values,
    }))
}
struct FileEnvironment {
    process: std::sync::Arc<dyn Environment>,
    path: PathBuf,
    values: BTreeMap<String, String>,
}
impl Environment for FileEnvironment {
    fn value(&self, name: &str) -> Result<Option<String>> {
        match self.process.value(name)? {
            Some(value) => Ok(Some(value)),
            None => Ok(self.values.get(name).cloned()),
        }
    }
    fn present(&self, name: &str) -> Result<bool> {
        if self.process.present(name)? {
            return Ok(true);
        }
        match self.process.value(name)? {
            Some(_) => Ok(false),
            None => Ok(self.values.get(name).is_some_and(|value| !value.is_empty())),
        }
    }
    fn path(&self, name: &str) -> Result<Option<PathBuf>> {
        Ok(self
            .process
            .path(name)?
            .or_else(|| self.values.get(name).map(PathBuf::from)))
    }
    fn source(&self, name: &str) -> crate::ConfigSource {
        if self.process.present(name).unwrap_or(false)
            || self.process.value(name).ok().flatten().is_some()
        {
            self.process.source(name)
        } else {
            crate::ConfigSource::EnvironmentFile {
                path: self.path.clone(),
                variable: name.into(),
            }
        }
    }
}
