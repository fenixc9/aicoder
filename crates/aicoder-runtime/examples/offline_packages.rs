//! Local bundles, frozen reload generations and public session state; no model or scripts.
use aicoder_core::{
    TurnExecutionContext,
    model::Message,
    session::{MemorySessionRepository, SessionRepository},
};
use aicoder_runtime::{packages::*, resources::ResourceSettings, *};
use serde_json::json;
use std::{collections::BTreeMap, fs, sync::Arc};
fn main() -> anyhow::Result<()> {
    let workspace = tempfile::tempdir()?;
    let home = tempfile::tempdir()?;
    let source = tempfile::tempdir()?;
    let manifest = json!({"schema_version":1,"id":"review","version":"1","files":["rules.md"],"resources":{"instructions":["rules.md"]},"permissions":{"instructions":true},"state":{"counter":{"type":"integer"}}});
    fs::write(
        source.path().join(MANIFEST_FILE),
        serde_json::to_vec(&manifest)?,
    )?;
    fs::write(source.path().join("rules.md"), "REVIEW_RULE_V1")?;
    let runtime = Runtime::load_with_environment(
        workspace.path(),
        RuntimeOverrides {
            settings: Settings {
                resources: ResourceSettings {
                    parents: Setting::Value(false),
                    nested: Setting::Value(false),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: false,
            ..Default::default()
        },
        Arc::new(BTreeMap::from([(
            "AICODER_HOME".into(),
            home.path().to_string_lossy().into_owned(),
        )])),
    )?;
    let store = runtime.package_store()?;
    let pin = store
        .install_local(
            source.path(),
            Default::default(),
            &TurnExecutionContext::new(),
        )?
        .pin;
    let old = runtime.resources()?;
    anyhow::ensure!(
        old.compose_system_messages(&[])
            .iter()
            .map(Message::text_content)
            .any(|text| text.contains("REVIEW_RULE_V1")),
        "package rules missing"
    );
    let mut state = PackageSessionState::capture(runtime.package_snapshot()?.as_ref());
    state.public.insert(
        "review".into(),
        BTreeMap::from([("counter".into(), json!(3))]),
    );
    let repo = MemorySessionRepository::new();
    let session = repo.create(workspace.path())?;
    state.save(
        &store,
        &repo,
        workspace.path(),
        &session.metadata().id,
        session.metadata().revision,
        &TurnExecutionContext::new(),
    )?;
    fs::write(source.path().join("rules.md"), "REVIEW_RULE_V2")?;
    store.install_local(
        source.path(),
        Default::default(),
        &TurnExecutionContext::new(),
    )?;
    let next = runtime.reload_resources()?;
    anyhow::ensure!(
        old.manifest().generation != next.resources()?.manifest().generation,
        "reload did not adopt new content"
    );
    let saved =
        PackageSessionState::restore(&store, &repo, workspace.path(), &session.metadata().id)?
            .unwrap();
    let restored = next.with_package_state(&saved)?;
    anyhow::ensure!(
        restored.package_snapshot()?.pins() == vec![pin.clone()],
        "exact pins were not restored"
    );
    store.uninstall("review")?;
    anyhow::ensure!(
        saved.validate(&store).is_err(),
        "history restored revoked trust"
    );
    anyhow::ensure!(
        old.compose_system_messages(&[])
            .iter()
            .map(Message::text_content)
            .any(|text| text.contains("REVIEW_RULE_V1")),
        "uninstall mutated existing snapshot"
    );
    println!(
        "{}",
        json!({"pin":pin,"reloaded":true,"restored_public_counter":saved.public["review"]["counter"],"revoked_restore_rejected":true,"network_requests":0,"scripts_started":0})
    );
    Ok(())
}
