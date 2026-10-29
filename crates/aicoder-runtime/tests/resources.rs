use aicoder_core::{
    model::*,
    session::{MemorySessionRepository, SessionEntryData, SessionRepository},
    *,
};
use aicoder_runtime::{resources::*, *};
use async_trait::async_trait;
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};
fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}
fn runtime(workspace: &Path, global: &Path, resources: ResourceSettings) -> Runtime {
    Runtime::load_with_environment(
        workspace,
        RuntimeOverrides {
            settings: Settings {
                model: Setting::Value("offline".into()),
                resources: ResourceSettings {
                    parents: Setting::Value(false),
                    ..resources
                },
                ..Default::default()
            },
            ..Default::default()
        },
        ConfigDiscovery::default(),
        Arc::new(BTreeMap::from([(
            "AICODER_HOME".into(),
            global.to_string_lossy().to_string(),
        )])),
    )
    .unwrap()
}
fn skill(directory: &Path, name: &str, description: &str, body: &str) {
    write(
        &directory.join(name).join("SKILL.md"),
        &format!("---\nname: {name}\ndescription: {description}\n---\n{body}"),
    );
}
fn systems(snapshot: &ResourceSnapshot) -> String {
    snapshot
        .compose_system_messages(&[Message::text(MessageRole::User, "work")])
        .iter()
        .filter(|message| message.role == MessageRole::System)
        .map(Message::text_content)
        .collect::<Vec<_>>()
        .join("\n")
}
#[test]
fn global_workspace_and_nested_agents_are_ordered_and_scope_is_visible_before_any_tool() {
    let global = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    write(&global.path().join("AGENTS.md"), "GLOBAL_RULE");
    write(&workspace.path().join("AGENTS.md"), "WORKSPACE_RULE");
    write(&workspace.path().join("src/AGENTS.md"), "NESTED_RULE");
    write(&workspace.path().join("target/AGENTS.md"), "IGNORED_RULE");
    let runtime = runtime(workspace.path(), global.path(), Default::default());
    let snapshot = runtime.resources().unwrap();
    let text = systems(&snapshot);
    assert!(text.find("GLOBAL_RULE").unwrap() < text.find("WORKSPACE_RULE").unwrap());
    assert!(text.find("WORKSPACE_RULE").unwrap() < text.find("NESTED_RULE").unwrap());
    assert!(text.contains(&workspace.path().join("src").to_string_lossy().to_string()));
    assert!(!text.contains("IGNORED_RULE"));
    assert_eq!(
        snapshot
            .manifest()
            .files
            .iter()
            .filter(|file| file.kind == ResourceKind::Agents)
            .count(),
        3
    );
}
#[test]
fn parent_instructions_can_be_enabled_separately_from_nested_discovery() {
    let global = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let workspace = parent.path().join("project");
    std::fs::create_dir(&workspace).unwrap();
    write(&parent.path().join("AGENTS.md"), "PARENT_RULE");
    let env = Arc::new(BTreeMap::from([(
        "AICODER_HOME".into(),
        global.path().to_string_lossy().to_string(),
    )]));
    let runtime = Runtime::load_with_environment(
        &workspace,
        RuntimeOverrides {
            settings: Settings {
                resources: ResourceSettings {
                    parents: Setting::Value(true),
                    nested: Setting::Value(false),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
        Default::default(),
        env,
    )
    .unwrap();
    assert!(systems(&runtime.resources().unwrap()).contains("PARENT_RULE"));
}
#[test]
fn nearest_system_replaces_the_base_and_explicit_configuration_wins_ties() {
    let global = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    write(&global.path().join("SYSTEM.md"), "GLOBAL_SYSTEM");
    write(&workspace.path().join("SYSTEM.md"), "PLAIN_SYSTEM");
    write(
        &workspace.path().join(".aicoder/SYSTEM.md"),
        "HIDDEN_SYSTEM",
    );
    write(&workspace.path().join("APPEND_SYSTEM.md"), "APPEND_RULE");
    let runtime = runtime(workspace.path(), global.path(), Default::default());
    let text = systems(&runtime.resources().unwrap());
    assert!(text.contains("HIDDEN_SYSTEM") && text.contains("APPEND_RULE"));
    assert!(!text.contains("GLOBAL_SYSTEM") && !text.contains("PLAIN_SYSTEM"));
    let configured = Runtime::load_with_environment(
        workspace.path(),
        RuntimeOverrides {
            settings: Settings {
                system_prompt: Setting::Value("EXPLICIT_BASE".into()),
                resources: ResourceSettings {
                    parents: Setting::Value(false),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
        Default::default(),
        Arc::new(BTreeMap::from([(
            "AICODER_HOME".into(),
            global.path().to_string_lossy().to_string(),
        )])),
    )
    .unwrap();
    let text = systems(&configured.resources().unwrap());
    assert!(text.contains("EXPLICIT_BASE") && !text.contains("HIDDEN_SYSTEM"));
}
#[test]
fn resource_discovery_is_lazy_and_disabled_policies_do_not_read_bad_documents() {
    let global = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    write(&workspace.path().join("AGENTS.md"), &"x".repeat(70000));
    let enabled = runtime(workspace.path(), global.path(), Default::default());
    assert!(enabled.resources().is_err());
    let disabled = runtime(
        workspace.path(),
        global.path(),
        ResourceSettings {
            enabled: Setting::Value(false),
            ..Default::default()
        },
    );
    assert!(disabled.resources().unwrap().manifest().files.is_empty());
    assert_eq!(disabled.resources().unwrap().manifest().scanned_entries, 0);
}
#[test]
fn skills_disclose_metadata_first_and_parse_real_yaml_without_executing_the_body() {
    let global = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let root = workspace.path().join(".aicoder/skills");
    write(
        &root.join("rust/SKILL.md"),
        "---\nname: rust\ndescription: >-\n  Help with Rust:\n  check build failures.\nmetadata:\n  category: language\n  labels: [rust, tooling]\n---\nSKILL_BODY_SENTINEL\nRun scripts/check.sh if useful.\n",
    );
    write(&root.join("rust/scripts/check.sh"), "touch must-not-run");
    let runtime = runtime(workspace.path(), global.path(), Default::default());
    let snapshot = runtime.resources().unwrap();
    let initial = systems(&snapshot);
    assert!(initial.contains("Help with Rust: check build failures."));
    assert!(!initial.contains("SKILL_BODY_SENTINEL"));
    assert!(
        snapshot
            .load_skill("rust", false)
            .unwrap()
            .contains("SKILL_BODY_SENTINEL")
    );
    assert!(!workspace.path().join("must-not-run").exists());
    assert!(snapshot.manifest().retained_bytes < snapshot.manifest().scanned_bytes);
}
#[test]
fn a_project_skill_shadows_a_global_name_and_manual_only_skills_stay_out_of_auto_catalogs() {
    let global = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    skill(
        &global.path().join("skills"),
        "review",
        "Global review",
        "GLOBAL_BODY",
    );
    skill(
        &workspace.path().join(".aicoder/skills"),
        "review",
        "Project review",
        "PROJECT_BODY",
    );
    write(
        &workspace.path().join(".aicoder/skills/manual/SKILL.md"),
        "---\nname: manual\ndescription: Manual instructions\ndisable-model-invocation: true\n---\nMANUAL_BODY",
    );
    let runtime = runtime(workspace.path(), global.path(), Default::default());
    let snapshot = runtime.resources().unwrap();
    assert!(
        snapshot
            .load_skill("review", false)
            .unwrap()
            .contains("PROJECT_BODY")
    );
    assert_eq!(
        snapshot
            .manifest()
            .files
            .iter()
            .filter(|file| file.name.as_deref() == Some("review") && file.active)
            .count(),
        1
    );
    assert!(!systems(&snapshot).contains("Manual instructions"));
    assert!(snapshot.load_skill("manual", false).is_err());
    assert!(
        snapshot
            .prepare_prompt("/skill:manual explicit task")
            .unwrap()
            .text
            .contains("MANUAL_BODY")
    );
}
#[test]
fn invalid_yaml_aliases_and_duplicate_skill_names_do_not_produce_partial_context() {
    let global = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join(".aicoder/skills/rust/SKILL.md");
    for text in [
        "---\nname: rust\ndescription: &alias text\nmetadata: *alias\n---\nbody",
        "---\nname: rust\ndescription: [not, text]\n---\nbody",
    ] {
        write(&path, text);
        assert!(
            runtime(workspace.path(), global.path(), Default::default())
                .resources()
                .is_err()
        );
    }
    skill(
        &workspace.path().join(".aicoder/skills"),
        "rust",
        "valid",
        "body",
    );
    skill(
        &workspace.path().join(".aicoder/skills/category"),
        "rust",
        "duplicate",
        "body",
    );
    assert!(
        runtime(workspace.path(), global.path(), Default::default())
            .resources()
            .is_err()
    );
}
#[test]
fn prompt_expansion_handles_quotes_positions_slices_and_literal_dollars_without_shell_execution() {
    let global = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    write(
        &workspace.path().join(".aicoder/prompts/review.md"),
        "---\ndescription: Review code\n---\nfirst=$1 second=${2}; all=$@; tail=${@:2}; slice=${@:1:2}; dollar=$$; literal=$HOME",
    );
    let runtime = runtime(workspace.path(), global.path(), Default::default());
    let prompt = runtime
        .prepare_prompt("/review 'two words' next final")
        .unwrap();
    assert!(prompt.text.contains("first=two words second=next"));
    assert!(prompt.text.contains("all=two words next final"));
    assert!(
        prompt
            .text
            .contains("tail=next final; slice=two words next")
    );
    assert!(prompt.text.contains("dollar=$; literal=$HOME"));
    assert!(prompt.resource.is_some());
    assert_eq!(
        runtime.prepare_prompt("//literal task").unwrap().text,
        "/literal task"
    );
    assert!(runtime.prepare_prompt("/review 'unclosed").is_err());
    assert!(runtime.prepare_prompt("/unknown").is_err());
}
#[test]
fn expansion_is_bounded_even_when_a_small_template_repeats_large_arguments() {
    let global = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    write(
        &workspace.path().join(".aicoder/prompts/repeat.md"),
        &"$1 ".repeat(500),
    );
    let runtime = runtime(
        workspace.path(),
        global.path(),
        ResourceSettings {
            max_total_bytes: Setting::Value(4096),
            ..Default::default()
        },
    );
    assert!(
        runtime
            .prepare_prompt(&format!("/repeat {}", "x".repeat(1000)))
            .is_err()
    );
}
#[test]
fn loaded_instructions_and_templates_are_frozen_and_changed_skill_bodies_require_reload() {
    let global = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let agents = workspace.path().join("AGENTS.md");
    let prompt = workspace.path().join(".aicoder/prompts/review.md");
    write(&agents, "OLD_RULE");
    write(&prompt, "OLD_TEMPLATE");
    skill(
        &workspace.path().join(".aicoder/skills"),
        "rust",
        "Rust help",
        "OLD_SKILL",
    );
    let runtime = runtime(workspace.path(), global.path(), Default::default());
    let old = runtime.resources().unwrap();
    write(&agents, "NEW_RULE");
    write(&prompt, "NEW_TEMPLATE");
    skill(
        &workspace.path().join(".aicoder/skills"),
        "rust",
        "Rust help",
        "NEW_SKILL",
    );
    assert!(systems(&old).contains("OLD_RULE"));
    assert!(
        old.prepare_prompt("/review")
            .unwrap()
            .text
            .contains("OLD_TEMPLATE")
    );
    assert!(old.load_skill("rust", false).is_err());
    let reloaded = runtime.reload_resources().unwrap();
    assert!(systems(&reloaded.resources().unwrap()).contains("NEW_RULE"));
    assert!(
        reloaded
            .prepare_prompt("/review")
            .unwrap()
            .text
            .contains("NEW_TEMPLATE")
    );
    assert_ne!(
        old.manifest().generation,
        reloaded.resources().unwrap().manifest().generation
    );
}
#[test]
fn file_count_scan_and_tree_depth_limits_fail_explicitly() {
    let global = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    write(&workspace.path().join("AGENTS.md"), "root");
    write(&workspace.path().join("child/AGENTS.md"), "child");
    assert!(
        runtime(
            workspace.path(),
            global.path(),
            ResourceSettings {
                max_files: Setting::Value(1),
                ..Default::default()
            }
        )
        .resources()
        .is_err()
    );
    assert!(
        runtime(
            workspace.path(),
            global.path(),
            ResourceSettings {
                max_entries: Setting::Value(1),
                ..Default::default()
            }
        )
        .resources()
        .is_err()
    );
    write(&workspace.path().join("child/deep/file"), "x");
    assert!(
        runtime(
            workspace.path(),
            global.path(),
            ResourceSettings {
                max_depth: Setting::Value(1),
                ..Default::default()
            }
        )
        .resources()
        .is_err()
    );
}
#[test]
fn skill_assets_use_bounded_utf8_pages_and_reject_parent_or_absolute_paths() {
    let global = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let root = workspace.path().join(".aicoder/skills");
    skill(&root, "rust", "Rust help", "body");
    write(&root.join("rust/references/note.txt"), "甲乙hello");
    let runtime = runtime(workspace.path(), global.path(), Default::default());
    let snapshot = runtime.resources().unwrap();
    let first = snapshot
        .read_skill_file("rust", Path::new("references/note.txt"), 0, 4)
        .unwrap();
    assert_eq!(first["content"], "甲");
    assert_eq!(first["next_offset_bytes"], 3);
    let second = snapshot
        .read_skill_file("rust", Path::new("references/note.txt"), 3, 4)
        .unwrap();
    assert_eq!(second["content"], "乙h");
    assert!(
        snapshot
            .read_skill_file("rust", Path::new("../secret"), 0, 32)
            .is_err()
    );
    assert!(
        snapshot
            .read_skill_file("rust", Path::new("/etc/passwd"), 0, 32)
            .is_err()
    );
    assert!(
        snapshot
            .read_skill_file("rust", Path::new(&"dir/".repeat(129)), 0, 32)
            .is_err()
    );
    assert!(
        snapshot
            .read_skill_file("rust", Path::new(&"x".repeat(4097)), 0, 32)
            .is_err()
    );
    assert!(
        snapshot
            .read_skill_file("rust", Path::new("references/note.txt"), 1, 4)
            .is_err()
    );
}
#[cfg(unix)]
#[test]
fn instruction_and_asset_links_cannot_read_outside_authorized_resource_roots() {
    use std::os::unix::fs::symlink;
    let global = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write(&outside.path().join("secret"), "DO_NOT_READ");
    symlink(
        outside.path().join("secret"),
        workspace.path().join("AGENTS.md"),
    )
    .unwrap();
    assert!(
        runtime(workspace.path(), global.path(), Default::default())
            .resources()
            .is_err()
    );
    std::fs::remove_file(workspace.path().join("AGENTS.md")).unwrap();
    let root = workspace.path().join(".aicoder/skills");
    skill(&root, "rust", "Rust help", "body");
    symlink(outside.path().join("secret"), root.join("rust/link")).unwrap();
    let runtime = runtime(workspace.path(), global.path(), Default::default());
    let snapshot = runtime.resources().unwrap();
    assert!(
        snapshot
            .read_skill_file("rust", Path::new("link"), 0, 32)
            .is_err()
    );
}
#[derive(Default)]
struct Model(Mutex<Vec<ModelRequest>>);
#[async_trait]
impl ModelProvider for Model {
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
        let model = request.model.clone();
        self.0.lock().unwrap().push(request);
        let response = ModelResponse {
            id: None,
            model,
            message: Message::text(MessageRole::Assistant, "done"),
            stop_reason: StopReason::Complete,
            usage: ModelUsage::zero(),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
#[tokio::test]
async fn actual_agent_input_gets_context_and_session_provenance_without_changing_user_history() {
    let global = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    write(&workspace.path().join("AGENTS.md"), "ACTUAL_PROJECT_RULE");
    let runtime = runtime(workspace.path(), global.path(), Default::default());
    let model = Arc::new(Model::default());
    let executor = runtime
        .executor_builder(model.clone())
        .unwrap()
        .build()
        .unwrap();
    let agent = Agent::new(executor, runtime.agent_config());
    let store = MemorySessionRepository::new();
    let outcome = agent
        .run_with_session(&store, SessionSelection::New, "USER_REQUEST", Arc::new(()))
        .await
        .unwrap();
    let requests = model.0.lock().unwrap();
    assert!(
        requests[0]
            .messages
            .iter()
            .any(|message| message.role == MessageRole::System
                && message.text_content().contains("ACTUAL_PROJECT_RULE"))
    );
    assert_eq!(
        requests[0].messages.last().unwrap().text_content(),
        "USER_REQUEST"
    );
    assert!(outcome.execution_result.progress.input_context.is_some());
    let session = store.open(&outcome.session.unwrap().id).unwrap();
    assert_eq!(session.chat_messages().len(), 2);
    assert!(session.entries().iter().any(|entry|matches!(&entry.data,SessionEntryData::Extension {namespace,data} if namespace=="aicoder.input_context"&&data["generation"].is_string())));
}
#[tokio::test]
async fn case_factories_isolate_project_parents_and_reuse_frozen_global_context() {
    let global = tempfile::tempdir().unwrap();
    let invocation = tempfile::tempdir().unwrap();
    let case_parent = tempfile::tempdir().unwrap();
    let case = case_parent.path().join("case");
    std::fs::create_dir(&case).unwrap();
    write(&global.path().join("AGENTS.md"), "GLOBAL_FROZEN");
    write(
        &invocation.path().join("AGENTS.md"),
        "INVOCATION_MUST_NOT_APPEAR",
    );
    write(
        &case_parent.path().join("AGENTS.md"),
        "PARENT_MUST_NOT_APPEAR",
    );
    write(&case.join("AGENTS.md"), "CASE_RULE");
    let runtime = runtime(invocation.path(), global.path(), Default::default());
    runtime.global_resources().unwrap();
    write(&global.path().join("AGENTS.md"), "GLOBAL_CHANGED");
    let model = Arc::new(Model::default());
    let executor = runtime
        .executor_builder_for(model.clone(), &case)
        .unwrap()
        .build()
        .unwrap();
    let mut request = ModelRequest::new(
        "offline",
        vec![
            Message::text(
                MessageRole::System,
                format!(
                    "{}\n\nBENCHMARK_TASK",
                    runtime.agent_config().system_prompt.unwrap()
                ),
            ),
            Message::text(MessageRole::User, "issue"),
        ],
    );
    request.options = runtime.config().options.clone();
    let outcome = executor.run(request).await.unwrap();
    let requests = model.0.lock().unwrap();
    let text = requests[0]
        .messages
        .iter()
        .map(Message::text_content)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("GLOBAL_FROZEN")
            && text.contains("CASE_RULE")
            && text.contains("BENCHMARK_TASK")
    );
    assert!(
        !text.contains("GLOBAL_CHANGED")
            && !text.contains("INVOCATION_MUST_NOT_APPEAR")
            && !text.contains("PARENT_MUST_NOT_APPEAR")
    );
    assert_eq!(
        outcome.progress.input_context.as_ref().unwrap()["workspace"],
        case.to_string_lossy().as_ref()
    );
}

#[test]
fn system_files_preserve_custom_host_messages_that_only_share_a_text_prefix() {
    let global = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    write(&workspace.path().join("SYSTEM.md"), "RESOURCE_SYSTEM");
    let runtime = runtime(workspace.path(), global.path(), Default::default());
    let host = Message::text(
        MessageRole::System,
        format!(
            "{}-custom-host-message",
            runtime.agent_config().system_prompt.unwrap()
        ),
    );
    let prepared = runtime
        .resources()
        .unwrap()
        .compose_system_messages(&[host.clone(), Message::text(MessageRole::User, "work")]);
    assert!(prepared.iter().any(|message| message == &host));
    assert!(
        prepared
            .iter()
            .any(|message| message.role == MessageRole::System
                && message.text_content() == "RESOURCE_SYSTEM")
    );
}

#[test]
fn disabled_global_resources_cannot_reenter_through_an_ancestor_hidden_directory() {
    let parent = tempfile::tempdir().unwrap();
    let workspace = parent.path().join("project");
    let global = parent.path().join(".aicoder");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir(&global).unwrap();
    write(&parent.path().join("AGENTS.md"), "PARENT_ALLOWED");
    write(&global.join("SYSTEM.md"), "GLOBAL_SYSTEM_DISABLED");
    write(&global.join("AGENTS.md"), "GLOBAL_AGENTS_DISABLED");
    skill(
        &global.join("skills"),
        "rust",
        "Global skill disabled",
        "body",
    );
    let runtime = Runtime::load_with_environment(
        &workspace,
        RuntimeOverrides {
            settings: Settings {
                resources: ResourceSettings {
                    global: Setting::Value(false),
                    parents: Setting::Value(true),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
        Default::default(),
        Arc::new(BTreeMap::from([(
            "AICODER_HOME".into(),
            global.to_string_lossy().to_string(),
        )])),
    )
    .unwrap();
    let snapshot = runtime.resources().unwrap();
    let text = systems(&snapshot);
    assert!(text.contains("PARENT_ALLOWED"));
    assert!(
        !text.contains("GLOBAL_SYSTEM_DISABLED")
            && !text.contains("Global skill disabled")
            && !text.contains("GLOBAL_AGENTS_DISABLED")
    );
    assert!(snapshot.load_skill("rust", false).is_err());
}

#[cfg(unix)]
#[test]
fn a_symlinked_trusted_global_root_is_not_rediscovered_as_an_unsafe_parent_collection() {
    use std::os::unix::fs::symlink;
    let parent = tempfile::tempdir().unwrap();
    let actual = tempfile::tempdir().unwrap();
    let workspace = parent.path().join("project");
    std::fs::create_dir(&workspace).unwrap();
    let global = parent.path().join(".aicoder");
    symlink(actual.path(), &global).unwrap();
    skill(&actual.path().join("skills"), "rust", "Global rust", "body");
    write(&actual.path().join("SYSTEM.md"), "GLOBAL_LINK_SYSTEM");
    let runtime = Runtime::load_with_environment(
        &workspace,
        RuntimeOverrides {
            settings: Settings {
                resources: ResourceSettings {
                    parents: Setting::Value(true),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
        Default::default(),
        Arc::new(BTreeMap::from([(
            "AICODER_HOME".into(),
            global.to_string_lossy().to_string(),
        )])),
    )
    .unwrap();
    let snapshot = runtime.resources().unwrap();
    assert!(systems(&snapshot).contains("GLOBAL_LINK_SYSTEM"));
    assert!(snapshot.load_skill("rust", false).is_ok());
    assert_eq!(
        snapshot
            .manifest()
            .files
            .iter()
            .filter(|file| file.kind == ResourceKind::Skill)
            .count(),
        1
    );
}
