use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path},
};
pub const MANIFEST_FILE: &str = "aicoder-package.json";
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackagePin {
    pub id: String,
    pub version: String,
    pub sha256: String,
}
impl PackagePin {
    pub fn validate(&self) -> Result<()> {
        name(&self.id)?;
        version(&self.version)?;
        ensure!(
            self.sha256.len() == 64
                && self
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid package content pin"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PackageResources {
    pub instructions: Vec<String>,
    pub skills: Vec<String>,
    pub prompts: Vec<String>,
    pub themes: Vec<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PackagePermissions {
    pub instructions: bool,
    pub execution: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramRequirement {
    pub name: String,
    pub version: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackageProgram {
    Bundled { path: String },
    Dependency { name: String },
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageProtocol {
    Process,
    Mcp,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageExecutable {
    pub id: String,
    pub protocol: PackageProtocol,
    pub program: PackageProgram,
    #[serde(default)]
    pub arguments: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum StateField {
    Boolean,
    Integer,
    String { max_bytes: usize },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageManifest {
    pub schema_version: u32,
    pub id: String,
    pub version: String,
    pub files: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<PackagePin>,
    #[serde(default)]
    pub programs: Vec<ProgramRequirement>,
    #[serde(default)]
    pub resources: PackageResources,
    #[serde(default)]
    pub permissions: PackagePermissions,
    #[serde(default)]
    pub executables: Vec<PackageExecutable>,
    #[serde(default)]
    pub state: BTreeMap<String, StateField>,
}
impl PackageManifest {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= MAX_MANIFEST_BYTES,
            "package manifest byte budget exceeded"
        );
        let manifest: Self = serde_json::from_slice(bytes)?;
        manifest.validate()?;
        Ok(manifest)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == 1,
            "unsupported package manifest version"
        );
        name(&self.id)?;
        version(&self.version)?;
        ensure!(
            self.files.len() <= 1024
                && self.dependencies.len() <= 32
                && self.programs.len() <= 32
                && self.executables.len() <= 32
                && self.state.len() <= 64,
            "package manifest count budget exceeded"
        );
        ensure!(
            self.files
                .iter()
                .map(|p| Path::new(p).components().count())
                .sum::<usize>()
                <= 4096,
            "package path component budget exceeded"
        );
        let mut files = BTreeSet::new();
        for file in &self.files {
            relative(file)?;
            ensure!(
                file != MANIFEST_FILE && files.insert(file.as_str()),
                "duplicate/recursive package file entry"
            );
            ensure!(
                !Path::new(file).components().any(|c| c
                    .as_os_str()
                    .to_str()
                    .is_some_and(|s| s == ".env" || s.starts_with(".env."))),
                "package cannot import environment files"
            );
        }
        let resources = [
            &self.resources.instructions,
            &self.resources.skills,
            &self.resources.prompts,
            &self.resources.themes,
        ];
        ensure!(
            resources.iter().map(|v| v.len()).sum::<usize>() <= 256,
            "package resource entry budget exceeded"
        );
        for resource in resources.into_iter().flatten() {
            relative(resource)?;
            ensure!(
                files.contains(resource.as_str()),
                "package resource is not in its file allowlist"
            );
        }
        ensure!(
            self.resources.instructions.is_empty() || self.permissions.instructions,
            "package instructions require a declared permission"
        );
        ensure!(
            self.executables.is_empty() || self.permissions.execution,
            "package executables require a declared permission"
        );
        let mut dependencies = BTreeSet::new();
        for pin in &self.dependencies {
            pin.validate()?;
            ensure!(
                pin.id != self.id && dependencies.insert(pin.id.as_str()),
                "duplicate/self package dependency"
            );
        }
        let mut programs = BTreeSet::new();
        for program in &self.programs {
            name(&program.name)?;
            version(&program.version)?;
            ensure!(
                programs.insert(program.name.as_str()),
                "duplicate program dependency"
            );
        }
        let mut entries = BTreeSet::new();
        for entry in &self.executables {
            name(&entry.id)?;
            ensure!(
                entries.insert(entry.id.as_str())
                    && entry.arguments.len() <= 128
                    && entry.arguments.iter().map(String::len).sum::<usize>() <= 64 * 1024
                    && !entry.arguments.iter().any(|a| a.contains('\0')),
                "invalid executable entry or argument budget"
            );
            match &entry.program {
                PackageProgram::Bundled { path } => {
                    relative(path)?;
                    ensure!(
                        files.contains(path.as_str()),
                        "bundled executable is undeclared"
                    );
                }
                PackageProgram::Dependency { name } => ensure!(
                    programs.contains(name.as_str()),
                    "undeclared program dependency"
                ),
            }
        }
        for (key, field) in &self.state {
            name(key)?;
            if let StateField::String { max_bytes } = field {
                ensure!(
                    (1..=4096).contains(max_bytes),
                    "package state field limit exceeded"
                );
            }
        }
        Ok(())
    }
    pub fn validate_state(&self, state: &BTreeMap<String, serde_json::Value>) -> Result<()> {
        ensure!(state.len() <= 64, "package state field budget exceeded");
        for (key, value) in state {
            let field = self
                .state
                .get(key)
                .ok_or_else(|| anyhow::anyhow!("undeclared package state field"))?;
            ensure!(
                match field {
                    StateField::Boolean => value.is_boolean(),
                    StateField::Integer => value.as_i64().is_some(),
                    StateField::String { max_bytes } =>
                        value.as_str().is_some_and(|s| s.len() <= *max_bytes),
                },
                "invalid public package state"
            );
        }
        Ok(())
    }
}
pub(crate) fn name(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            && value != "."
            && value != "..",
        "invalid package identifier"
    );
    Ok(())
}
pub(crate) fn version(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty() && value.len() <= 128 && value.bytes().all(|b| b.is_ascii_graphic()),
        "invalid package version"
    );
    Ok(())
}
pub(crate) fn relative(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control),
        "invalid package path"
    );
    ensure!(
        Path::new(value).components().count() <= 128
            && Path::new(value)
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
        "package path must stay relative without traversal"
    );
    Ok(())
}
