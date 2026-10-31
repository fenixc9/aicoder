use aicoder_core::{TurnExecutionContext, model::*, session::*};
use aicoder_runtime::{
    packages::*, resources::ResourceSettings, session::JsonlSessionRepository, *,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
    sync::Arc,
};
fn write(root: &Path, path: &str, bytes: impl AsRef<[u8]>) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}
fn manifest(root: &Path, id: &str, version: &str) -> Value {
    write(root, "instructions.md", format!("RULE_{version}"));
    write(
        root,
        "skills/demo/SKILL.md",
        "---\nname: demo\ndescription: Package demo\n---\nBODY",
    );
    write(root, "skills/demo/asset.txt", "甲乙abc");
    write(root, "prompts/review.md", "Review $1");
    let value = json!({"schema_version":1,"id":id,"version":version,
        "files":["instructions.md","skills/demo/SKILL.md","skills/demo/asset.txt","prompts/review.md"],
        "permissions":{"instructions":true},
        "resources":{"instructions":["instructions.md"],"skills":["skills/demo/SKILL.md"],"prompts":["prompts/review.md"]},
        "state":{"counter":{"type":"integer"},"label":{"type":"string","max_bytes":8}}});
    save(root, &value);
    value
}
fn save(root: &Path, value: &Value) {
    write(root, MANIFEST_FILE, serde_json::to_vec(value).unwrap());
}
fn install(store: &PackageStore, source: &Path) -> PackageRecord {
    store
        .install_local(source, Default::default(), &TurnExecutionContext::new())
        .unwrap()
}
fn runtime(workspace: &Path, home: &Path) -> Runtime {
    Runtime::load_with_environment(
        workspace,
        RuntimeOverrides {
            settings: Settings {
                model: Setting::Value("offline".into()),
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
            home.to_string_lossy().into_owned(),
        )])),
    )
    .unwrap()
}
fn text(runtime: &Runtime) -> String {
    runtime
        .resources()
        .unwrap()
        .compose_system_messages(&[])
        .iter()
        .map(Message::text_content)
        .collect::<Vec<_>>()
        .join("\n")
}
#[test]
fn content_pins_are_deterministic_and_import_only_allowlisted_files() {
    let source = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    manifest(source.path(), "demo", "1");
    write(source.path(), "not-imported.txt", "unlisted");
    let store = PackageStore::open(home.path()).unwrap();
    let first = install(&store, source.path());
    let second = install(&store, source.path());
    assert_eq!(first.pin, second.pin);
    let snapshot = store.snapshot(None, None).unwrap();
    let object = snapshot.get("demo").unwrap();
    assert!(!object.root().join("not-imported.txt").exists());
    assert!(object.read_asset("not-imported.txt", 100).is_err());
    assert_eq!(
        object.read_asset("instructions.md", 100).unwrap(),
        b"RULE_1"
    );
    assert_eq!(snapshot.pins(), vec![first.pin]);
    assert_eq!(
        fs::metadata(home.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );
}
#[test]
fn reload_preserves_old_generation_and_project_precedence() {
    let source = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let runtime = runtime(workspace.path(), home.path());
    let store = runtime.package_store().unwrap();
    manifest(source.path(), "demo", "1");
    let first = install(&store, source.path());
    assert!(text(&runtime).contains("RULE_1"));
    let old = runtime.resources().unwrap();
    manifest(source.path(), "demo", "2");
    install(&store, source.path());
    assert!(text(&runtime).contains("RULE_1"));
    let next = runtime.reload_resources().unwrap();
    assert!(text(&next).contains("RULE_2"));
    assert_ne!(
        old.manifest().generation,
        next.resources().unwrap().manifest().generation
    );
    let restored = runtime
        .with_package_state(&PackageSessionState {
            version: 1,
            pins: vec![first.pin],
            public: BTreeMap::new(),
        })
        .unwrap();
    assert!(text(&restored).contains("RULE_1"));
    write(
        workspace.path(),
        ".aicoder/prompts/review.md",
        "WORKSPACE $1",
    );
    write(
        workspace.path(),
        ".aicoder/skills/demo/SKILL.md",
        "---\nname: demo\ndescription: Workspace demo\n---\nLOCAL",
    );
    let local = next.reload_resources().unwrap();
    assert!(
        local
            .resources()
            .unwrap()
            .load_skill("demo", false)
            .unwrap()
            .contains("LOCAL")
    );
    assert!(
        serde_json::to_string(&local.prepare_prompt("/review change").unwrap())
            .unwrap()
            .contains("WORKSPACE")
    );
    store.uninstall("demo").unwrap();
    assert!(text(&next).contains("RULE_2"));
    assert!(old.load_skill("demo", false).unwrap().contains("BODY"));
    assert!(
        runtime
            .reload_resources()
            .unwrap()
            .package_snapshot()
            .unwrap()
            .pins()
            .is_empty()
    );
}
#[test]
fn pinned_skill_pages_reject_tampering_links_and_undeclared_assets() {
    let source = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    manifest(source.path(), "demo", "1");
    let runtime = runtime(workspace.path(), home.path());
    let store = runtime.package_store().unwrap();
    install(&store, source.path());
    let resources = runtime.resources().unwrap();
    let page = resources
        .read_skill_file("demo", Path::new("asset.txt"), 0, 4)
        .unwrap();
    assert_eq!(page["content"], "甲");
    assert_eq!(page["next_offset_bytes"], 3);
    let next = resources
        .read_skill_file("demo", Path::new("asset.txt"), 3, 8)
        .unwrap();
    assert_eq!(next["content"], "乙abc");
    assert_eq!(next["has_more"], false);
    assert!(
        resources
            .read_skill_file("demo", Path::new("../SKILL.md"), 0, 10)
            .is_err()
    );
    assert!(
        resources
            .read_skill_file("demo", Path::new("asset.txt"), 1, 10)
            .is_err()
    );
    assert!(
        resources
            .read_skill_file("demo", Path::new("asset.txt"), 100, 10)
            .is_err()
    );
    let packages = runtime.package_snapshot().unwrap();
    let root = packages.get("demo").unwrap().root();
    write(root, "skills/demo/undeclared.txt", "injected");
    assert!(
        resources
            .read_skill_file("demo", Path::new("undeclared.txt"), 0, 20)
            .is_err()
    );
    fs::remove_file(root.join("skills/demo/undeclared.txt")).unwrap();
    write(root, "skills/demo/asset.txt", "changed");
    assert!(
        resources
            .read_skill_file("demo", Path::new("asset.txt"), 0, 20)
            .is_err()
    );
    assert!(runtime.reload_resources().is_err());
    assert!(text(&runtime).contains("RULE_1"));
    fs::remove_file(root.join("skills/demo/asset.txt")).unwrap();
    symlink(
        source.path().join("instructions.md"),
        root.join("skills/demo/asset.txt"),
    )
    .unwrap();
    assert!(
        resources
            .read_skill_file("demo", Path::new("asset.txt"), 0, 20)
            .is_err()
    );
}
#[test]
fn invalid_or_cancelled_install_cannot_replace_registry_or_leave_staging() {
    let source = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut value = manifest(source.path(), "demo", "1");
    let store = PackageStore::open(home.path()).unwrap();
    let first = install(&store, source.path());
    let revision = store.index().unwrap().revision;
    value["version"] = json!("bad");
    save(source.path(), &value);
    write(source.path(), "skills/demo/SKILL.md", "no frontmatter");
    assert!(
        store
            .install_local(
                source.path(),
                Default::default(),
                &TurnExecutionContext::new()
            )
            .is_err()
    );
    assert_eq!(store.index().unwrap().revision, revision);
    assert_eq!(
        fs::read_dir(home.path().join("objects")).unwrap().count(),
        1
    );
    assert_eq!(
        fs::read_dir(home.path().join("staging")).unwrap().count(),
        0
    );
    manifest(source.path(), "demo", "2");
    let cancelled = TurnExecutionContext::new();
    cancelled.cancel("test cancellation");
    assert!(
        store
            .install_local(source.path(), Default::default(), &cancelled)
            .is_err()
    );
    assert_eq!(store.snapshot(None, None).unwrap().pins(), vec![first.pin]);
}
#[test]
fn executable_consent_is_explicit_and_install_never_launches_code() {
    let source = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut value = manifest(source.path(), "demo", "1");
    value["permissions"]["execution"] = json!(true);
    value["executables"] =
        json!([{"id":"tool","protocol":"process","program":{"kind":"bundled","path":"program"}}]);
    value["files"]
        .as_array_mut()
        .unwrap()
        .push(json!("program"));
    write(
        source.path(),
        "program",
        "#!/bin/sh\ntouch SHOULD_NOT_RUN\n",
    );
    fs::set_permissions(
        source.path().join("program"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    save(source.path(), &value);
    let store = PackageStore::open(home.path()).unwrap();
    assert!(
        store
            .install_local(
                source.path(),
                Default::default(),
                &TurnExecutionContext::new()
            )
            .is_err()
    );
    assert!(store.index().unwrap().installed.is_empty());
    let record = store
        .install_local(
            source.path(),
            InstallOptions {
                allow_execution: true,
                enable: true,
            },
            &TurnExecutionContext::new(),
        )
        .unwrap();
    assert!(record.execution_allowed);
    let snapshot = store.snapshot(None, None).unwrap();
    assert!(
        !snapshot
            .get("demo")
            .unwrap()
            .root()
            .join("SHOULD_NOT_RUN")
            .exists()
    );
    store.uninstall("demo").unwrap();
    assert!(snapshot.validate_trust(&store).is_err());
}
#[test]
fn declared_paths_reject_links_special_files_secrets_and_escape() {
    let source = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let base = manifest(source.path(), "demo", "1");
    let store = PackageStore::open(home.path()).unwrap();
    for path in ["../escape", "/escape", ".env", "dir/.env.local"] {
        let mut value = base.clone();
        value["files"].as_array_mut().unwrap().push(json!(path));
        save(source.path(), &value);
        assert!(PackageStore::inspect(source.path()).is_err(), "{path}");
    }
    save(source.path(), &base);
    fs::remove_file(source.path().join("instructions.md")).unwrap();
    symlink("prompts/review.md", source.path().join("instructions.md")).unwrap();
    assert!(
        store
            .install_local(
                source.path(),
                Default::default(),
                &TurnExecutionContext::new()
            )
            .is_err()
    );
    fs::remove_file(source.path().join("instructions.md")).unwrap();
    let path = std::ffi::CString::new(
        source
            .path()
            .join("instructions.md")
            .as_os_str()
            .as_encoded_bytes(),
    )
    .unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    assert!(
        store
            .install_local(
                source.path(),
                Default::default(),
                &TurnExecutionContext::new()
            )
            .is_err()
    );
    assert!(store.index().unwrap().installed.is_empty());
}
#[test]
fn file_and_disk_budgets_are_enforced_before_publication() {
    let source = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    manifest(source.path(), "demo", "1");
    let mut limits = PackageLimits::default();
    limits.max_file_bytes = 8;
    let store = PackageStore::with_limits(home.path(), limits).unwrap();
    assert!(
        store
            .install_local(
                source.path(),
                Default::default(),
                &TurnExecutionContext::new()
            )
            .is_err()
    );
    assert!(store.index().unwrap().installed.is_empty());
    let mut limits = PackageLimits::default();
    limits.max_objects = 1;
    let store = PackageStore::with_limits(home.path(), limits).unwrap();
    install(&store, source.path());
    manifest(source.path(), "demo", "2");
    assert!(
        store
            .install_local(
                source.path(),
                Default::default(),
                &TurnExecutionContext::new()
            )
            .is_err()
    );
    assert_eq!(
        fs::read_dir(home.path().join("objects")).unwrap().count(),
        1
    );
}
#[test]
fn dependencies_pin_exact_content_and_protect_transitive_trust() {
    let source = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let store = PackageStore::open(home.path()).unwrap();
    let mut a = manifest(source.path(), "a", "1");
    a["resources"] = json!({});
    save(source.path(), &a);
    let a1 = install(&store, source.path());
    store.enable("a", false).unwrap();
    let mut b = manifest(source.path(), "b", "1");
    b["resources"] = json!({});
    b["dependencies"] = json!([a1.pin]);
    save(source.path(), &b);
    let b1 = install(&store, source.path());
    store.enable("b", false).unwrap();
    let mut c = manifest(source.path(), "c", "1");
    c["resources"] = json!({});
    c["dependencies"] = json!([b1.pin]);
    save(source.path(), &c);
    install(&store, source.path());
    assert_eq!(store.snapshot(None, None).unwrap().packages().len(), 3);
    assert!(store.uninstall("a").is_err());
    let mut a = manifest(source.path(), "a", "2");
    a["resources"] = json!({});
    save(source.path(), &a);
    let before = store.index().unwrap().revision;
    assert!(
        store
            .install_local(
                source.path(),
                Default::default(),
                &TurnExecutionContext::new()
            )
            .is_err()
    );
    assert_eq!(store.index().unwrap().revision, before);
    let a2 = store
        .install_local(
            source.path(),
            InstallOptions {
                enable: false,
                ..Default::default()
            },
            &TurnExecutionContext::new(),
        )
        .unwrap();
    assert_ne!(a1.pin.sha256, a2.pin.sha256);
    assert!(store.enable("a", true).is_err());
    store.select_revision(&a1.pin).unwrap();
    store.enable("a", true).unwrap();
    let snapshot = store.snapshot(None, None).unwrap();
    assert_eq!(snapshot.get("a").unwrap().pin, a1.pin);
    store.enable("c", false).unwrap();
    store.enable("a", false).unwrap();
    store.uninstall("a").unwrap();
    assert!(store.enable("c", true).is_err());
}
#[test]
fn missing_dependencies_and_duplicate_package_resources_fail_closed() {
    let source = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let runtime = runtime(workspace.path(), home.path());
    let store = runtime.package_store().unwrap();
    let mut value = manifest(source.path(), "first", "1");
    value["dependencies"] = json!([{"id":"missing","version":"1","sha256":"a".repeat(64)}]);
    save(source.path(), &value);
    assert!(
        store
            .install_local(
                source.path(),
                Default::default(),
                &TurnExecutionContext::new()
            )
            .is_err()
    );
    value["dependencies"] = json!([]);
    save(source.path(), &value);
    install(&store, source.path());
    assert!(text(&runtime).contains("RULE_1"));
    manifest(source.path(), "second", "2");
    install(&store, source.path());
    assert!(runtime.reload_resources().is_err());
    assert!(text(&runtime).contains("RULE_1"));
    store.enable("second", false).unwrap();
    assert!(runtime.reload_resources().is_ok());
}
fn session_case(repository: &dyn SessionRepository, store: &PackageStore, workspace: &Path) {
    let snapshot = store.snapshot(None, None).unwrap();
    let mut state = PackageSessionState::capture(&snapshot);
    state.public.insert(
        "demo".into(),
        BTreeMap::from([("counter".into(), json!(3))]),
    );
    let mut session = repository.create(workspace).unwrap();
    let id = session.metadata().id.clone();
    repository
        .append(
            &mut session,
            Message::text(MessageRole::User, "keep canonical history"),
        )
        .unwrap();
    state
        .save(
            store,
            repository,
            workspace,
            &id,
            session.metadata().revision,
            &TurnExecutionContext::new(),
        )
        .unwrap();
    let saved = repository.open(&id).unwrap();
    let saved_leaf = saved.metadata().leaf_id.clone().unwrap();
    let restored = PackageSessionState::restore(store, repository, workspace, &id)
        .unwrap()
        .unwrap();
    assert_eq!(restored.public["demo"]["counter"], 3);
    assert_eq!(
        repository.open(&id).unwrap().model_messages().unwrap(),
        vec![Message::text(MessageRole::User, "keep canonical history")]
    );
    assert!(
        state
            .save(
                store,
                repository,
                workspace,
                &id,
                0,
                &TurnExecutionContext::new()
            )
            .is_err()
    );
    let mut run = repository.open_run(&id).unwrap();
    let lease = run.acquire(repository).unwrap();
    assert!(
        state
            .save(
                store,
                repository,
                workspace,
                &id,
                run.metadata().revision,
                &TurnExecutionContext::new()
            )
            .is_err()
    );
    drop(lease);
    let mut branch = repository.open(&id).unwrap();
    repository.checkout(&mut branch, None).unwrap();
    assert!(
        PackageSessionState::restore(store, repository, workspace, &id)
            .unwrap()
            .is_none()
    );
    repository.checkout(&mut branch, Some(&saved_leaf)).unwrap();
    assert!(
        repository
            .extension_state(&id, PACKAGE_STATE_NAMESPACE, 8)
            .is_err()
    );
    state
        .public
        .get_mut("demo")
        .unwrap()
        .insert("grant".into(), json!(true));
    assert!(state.validate(store).is_err());
    state.public.get_mut("demo").unwrap().remove("grant");
    state
        .public
        .get_mut("demo")
        .unwrap()
        .insert("label".into(), json!("too long for field"));
    assert!(state.validate(store).is_err());
    store.uninstall("demo").unwrap();
    assert!(PackageSessionState::restore(store, repository, workspace, &id).is_err());
    assert!(
        snapshot
            .get("demo")
            .unwrap()
            .read_asset("instructions.md", 100)
            .is_ok()
    );
}
#[test]
fn session_state_restores_selected_branch_with_current_host_trust_in_memory() {
    let source = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    manifest(source.path(), "demo", "1");
    let store = PackageStore::open(home.path()).unwrap();
    install(&store, source.path());
    session_case(&MemorySessionRepository::new(), &store, workspace.path());
}
#[test]
fn session_state_materializes_real_cold_archive_records_without_loading_full_history() {
    let source = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    manifest(source.path(), "demo", "1");
    let store = PackageStore::open(home.path()).unwrap();
    install(&store, source.path());
    let repo = JsonlSessionRepository::new(sessions.path()).unwrap();
    session_case(&repo, &store, workspace.path());
    install(&store, source.path());
    let mut session = repo.create(workspace.path()).unwrap();
    let id = session.metadata().id.clone();
    repo.append(
        &mut session,
        Message::text(MessageRole::User, "x".repeat(8192)),
    )
    .unwrap();
    let state = PackageSessionState::capture(&store.snapshot(None, None).unwrap());
    state
        .save(
            &store,
            &repo,
            workspace.path(),
            &id,
            session.metadata().revision,
            &TurnExecutionContext::new(),
        )
        .unwrap();
    let limited = JsonlSessionRepository::with_limits(
        sessions.path(),
        aicoder_runtime::session::SessionLoadLimits {
            max_snapshot_bytes: 1024,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(limited.open(&id).is_err());
    let restored = PackageSessionState::restore(&store, &limited, workspace.path(), &id)
        .unwrap()
        .unwrap();
    assert_eq!(restored.pins, state.pins);
}
#[test]
fn project_settings_cannot_define_storage_or_host_program_profiles() {
    for content in [
        r#"{"packages":{"root":"/tmp/untrusted"}}"#,
        r#"{"packages":{"programs":{"shell":"/bin/sh"}}}"#,
    ] {
        assert!(serde_json::from_str::<Settings>(content).is_err());
    }
}

#[cfg(feature = "process-extension-fixtures")]
mod process_activation {
    use super::*;
    use aicoder_core::{
        tools::{AllowAllApproval, DenyAllApproval},
        *,
    };
    use async_trait::async_trait;
    struct Provider {
        tool: String,
    }
    #[async_trait]
    impl ModelProvider for Provider {
        fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
            let mut spec = ModelSpec::new("offline", model);
            spec.capabilities.tools = true;
            Ok(spec)
        }
        async fn generate(
            &self,
            request: ModelRequest,
            sink: Arc<dyn ModelEventSink>,
            _: TurnExecutionContext,
        ) -> Result<ModelResponse, ProviderError> {
            let mut response = ModelResponse {
                id: None,
                model: request.model,
                message: Message::text(MessageRole::Assistant, "done"),
                stop_reason: StopReason::Complete,
                usage: ModelUsage::zero(),
            };
            if request.messages.last().unwrap().role == MessageRole::User {
                response.stop_reason = StopReason::ToolCalls;
                response.message.content = vec![ContentBlock::ToolCall {
                    call: ModelToolCall {
                        id: "one".into(),
                        name: self.tool.clone(),
                        arguments: json!({"path":"note.txt","content":"after"}),
                    },
                }];
            }
            emit_response(&response, sink.as_ref());
            Ok(response)
        }
    }
    fn executable_manifest(root: &Path) -> Value {
        let mut value = manifest(root, "demo", "1");
        value["permissions"]["execution"] = json!(true);
        value["programs"] = json!([{"name":"fixture","version":"1"}]);
        value["executables"] = json!([{"id":"peer","protocol":"process","program":{"kind":"dependency","name":"fixture"},"arguments":["normal","{workspace}/peer.pid"]}]);
        save(root, &value);
        value
    }
    fn profiles() -> PackagePrograms {
        let mut programs = PackagePrograms::new(Arc::new(BTreeMap::<String, String>::new()));
        programs
            .register(
                "fixture",
                HostProgram::new(env!("CARGO_BIN_EXE_aicoder-process-fixture"), "1"),
            )
            .unwrap();
        programs
    }
    fn install_code(store: &PackageStore, source: &Path) {
        store
            .install_local(
                source,
                InstallOptions {
                    allow_execution: true,
                    enable: true,
                },
                &TurnExecutionContext::new(),
            )
            .unwrap();
    }
    #[tokio::test]
    async fn packages_activate_real_tools_and_execution_consent_does_not_bypass_approval() {
        let source = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        executable_manifest(source.path());
        let store = PackageStore::open(home.path()).unwrap();
        install_code(&store, source.path());
        write(workspace.path(), "note.txt", "before");
        let snapshot = store.snapshot(None, None).unwrap();
        let activation = snapshot
            .activate(
                &store,
                &profiles(),
                workspace.path(),
                TurnExecutionContext::new(),
            )
            .await
            .unwrap();
        let extensions = activation.extensions();
        let tool = extensions
            .catalog()
            .registrations
            .into_iter()
            .find(|r| r.name.ends_with("__write"))
            .unwrap()
            .name;
        let denied = Agent::new(
            TurnExecutor::builder(Provider { tool: tool.clone() })
                .workspace(workspace.path())
                .extensions(extensions.clone())
                .approval(DenyAllApproval)
                .build()
                .unwrap(),
            AgentConfig::new("offline"),
        );
        denied.run("write", Arc::new(())).await.unwrap();
        assert_eq!(
            fs::read_to_string(workspace.path().join("note.txt")).unwrap(),
            "before"
        );
        let approved = Agent::new(
            TurnExecutor::builder(Provider { tool })
                .workspace(workspace.path())
                .extensions(extensions)
                .approval(AllowAllApproval)
                .build()
                .unwrap(),
            AgentConfig::new("offline"),
        );
        approved.run("write", Arc::new(())).await.unwrap();
        assert_eq!(
            fs::read_to_string(workspace.path().join("note.txt")).unwrap(),
            "after"
        );
        activation.shutdown().await.unwrap();
        assert_dead(workspace.path().join("peer.pid").as_path());
    }
    fn assert_dead(path: &Path) {
        let pid = fs::read_to_string(path).unwrap();
        assert!(
            !Path::new(&format!("/proc/{}/stat", pid.trim())).exists(),
            "child owner must be reaped"
        );
    }
    #[tokio::test]
    async fn failed_partial_activation_reaps_started_owners_and_keeps_old_snapshot() {
        let source = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut value = executable_manifest(source.path());
        let store = PackageStore::open(home.path()).unwrap();
        install_code(&store, source.path());
        let old = store.snapshot(None, None).unwrap();
        let live = old
            .activate(
                &store,
                &profiles(),
                workspace.path(),
                TurnExecutionContext::new(),
            )
            .await
            .unwrap();
        value["version"] = json!("2");
        value["executables"][0]["arguments"] = json!(["normal", "{workspace}/first.pid"]);
        value["executables"].as_array_mut().unwrap().push(json!({"id":"broken","protocol":"process","program":{"kind":"dependency","name":"fixture"},"arguments":["bad_json","{workspace}/second.pid"]}));
        save(source.path(), &value);
        install_code(&store, source.path());
        let next = store.snapshot(None, None).unwrap();
        assert!(
            next.activate(
                &store,
                &profiles(),
                workspace.path(),
                TurnExecutionContext::new()
            )
            .await
            .is_err()
        );
        assert_dead(&workspace.path().join("first.pid"));
        assert_dead(&workspace.path().join("second.pid"));
        assert!(!live.extensions().catalog().registrations.is_empty());
        assert!(old.validate_trust(&store).is_ok());
        live.shutdown().await.unwrap();
        assert_dead(&workspace.path().join("peer.pid"));
    }
    #[tokio::test]
    async fn host_profile_validation_precedes_any_process_launch() {
        let source = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut value = executable_manifest(source.path());
        value["programs"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":"missing","version":"1"}));
        value["executables"].as_array_mut().unwrap().push(json!({"id":"other","protocol":"process","program":{"kind":"dependency","name":"missing"}}));
        save(source.path(), &value);
        let store = PackageStore::open(home.path()).unwrap();
        install_code(&store, source.path());
        let snapshot = store.snapshot(None, None).unwrap();
        assert!(
            snapshot
                .activate(
                    &store,
                    &profiles(),
                    workspace.path(),
                    TurnExecutionContext::new()
                )
                .await
                .is_err()
        );
        assert!(!workspace.path().join("peer.pid").exists());
    }
    #[cfg(feature = "mcp-fixtures")]
    #[tokio::test]
    async fn package_mcp_entries_use_the_optional_existing_adapter() {
        let source = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut value = executable_manifest(source.path());
        value["executables"][0]["protocol"] = json!("mcp");
        value["executables"][0]["arguments"] = json!([]);
        save(source.path(), &value);
        let store = PackageStore::open(home.path()).unwrap();
        install_code(&store, source.path());
        let mut profiles = PackagePrograms::new(Arc::new(BTreeMap::<String, String>::new()));
        profiles
            .register(
                "fixture",
                HostProgram::new(env!("CARGO_BIN_EXE_aicoder-mcp-fixture"), "1"),
            )
            .unwrap();
        let activation = store
            .snapshot(None, None)
            .unwrap()
            .activate(
                &store,
                &profiles,
                workspace.path(),
                TurnExecutionContext::new(),
            )
            .await
            .unwrap();
        assert!(!activation.extensions().catalog().registrations.is_empty());
        activation.shutdown().await.unwrap();
    }
}

#[test]
fn explicitly_disabled_install_disables_previous_active_root() {
    let source = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    manifest(source.path(), "demo", "1");
    let store = PackageStore::open(home.path()).unwrap();
    install(&store, source.path());
    manifest(source.path(), "demo", "2");
    store
        .install_local(
            source.path(),
            InstallOptions {
                enable: false,
                ..Default::default()
            },
            &TurnExecutionContext::new(),
        )
        .unwrap();
    assert!(store.snapshot(None, None).unwrap().pins().is_empty());
    assert_eq!(store.index().unwrap().installed.len(), 2);
}
#[test]
fn concurrent_installations_publish_complete_registry_generations() {
    let home = tempfile::tempdir().unwrap();
    let store = Arc::new(PackageStore::open(home.path()).unwrap());
    let threads: Vec<_> = (0..6)
        .map(|i| {
            let store = store.clone();
            std::thread::spawn(move || {
                let source = tempfile::tempdir().unwrap();
                let mut value = manifest(source.path(), &format!("p{i}"), "1");
                value["resources"] = json!({});
                save(source.path(), &value);
                install(&store, source.path());
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    let index = store.index().unwrap();
    assert_eq!(index.revision, 6);
    assert_eq!(index.installed.len(), 6);
    assert_eq!(store.snapshot(None, None).unwrap().packages().len(), 6);
}
#[test]
fn global_discovery_can_be_disabled_while_explicit_host_package_selection_still_works() {
    let source = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    manifest(source.path(), "demo", "1");
    let rt = runtime(workspace.path(), home.path());
    install(&rt.package_store().unwrap(), source.path());
    let env: Arc<dyn Environment> = Arc::new(BTreeMap::from([
        (
            "AICODER_HOME".into(),
            home.path().to_string_lossy().into_owned(),
        ),
        ("AICODER_GLOBAL_CONTEXT".into(), "false".into()),
    ]));
    let settings = RuntimeOverrides::default();
    let discovery = ConfigDiscovery {
        enabled: false,
        ..Default::default()
    };
    let hidden = Runtime::load_with_environment(
        workspace.path(),
        settings.clone(),
        discovery.clone(),
        env.clone(),
    )
    .unwrap();
    assert!(hidden.package_snapshot().unwrap().pins().is_empty());
    let selected = Runtime::load_with_environment(
        workspace.path(),
        RuntimeOverrides {
            settings: Settings {
                packages: PackageSettings {
                    selected: Setting::Value(vec!["demo".into()]),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..settings
        },
        discovery,
        env,
    )
    .unwrap();
    assert!(text(&selected).contains("RULE_1"));
}
#[test]
fn selection_environment_is_bounded_and_settings_precedence_is_preserved() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let env: Arc<dyn Environment> = Arc::new(BTreeMap::from([
        (
            "AICODER_HOME".into(),
            home.path().to_string_lossy().into_owned(),
        ),
        ("AICODER_PACKAGES".into(), "[\"missing\"]".into()),
    ]));
    let discovery = ConfigDiscovery {
        enabled: false,
        ..Default::default()
    };
    let missing = Runtime::load_with_environment(
        workspace.path(),
        RuntimeOverrides::default(),
        discovery.clone(),
        env.clone(),
    )
    .unwrap();
    assert!(missing.package_snapshot().is_err());
    let explicit = Runtime::load_with_environment(
        workspace.path(),
        RuntimeOverrides {
            settings: Settings {
                packages: PackageSettings {
                    selected: Setting::Value(vec![]),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
        discovery,
        env,
    )
    .unwrap();
    assert!(explicit.package_snapshot().unwrap().pins().is_empty());
    assert!(!home.path().join("packages").exists());
}

#[test]
fn extension_query_preserves_bounded_legacy_read_compatibility() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let repo = JsonlSessionRepository::new(home.path()).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let original = format!(
        "{}\n{}\n",
        json!({"type":"session","version":1,"id":id,"created_at":1,"cwd":workspace.path()}),
        json!({"type":"message","id":"user","created_at":2,"message":{"role":"user","content":"remember"}})
    );
    let path = home.path().join("legacy.jsonl");
    fs::write(&path, &original).unwrap();
    assert!(
        repo.extension_state(&id, PACKAGE_STATE_NAMESPACE, 32768)
            .unwrap()
            .is_none()
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    let limited = JsonlSessionRepository::with_limits(
        home.path(),
        aicoder_runtime::session::SessionLoadLimits {
            max_snapshot_bytes: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        limited
            .extension_state(&id, PACKAGE_STATE_NAMESPACE, 32768)
            .is_err()
    );
}
