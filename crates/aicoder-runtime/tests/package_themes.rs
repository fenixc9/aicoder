use aicoder_core::TurnExecutionContext;
use aicoder_runtime::{packages::*, resources::ResourceSettings, *};
use serde_json::json;
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt, path::Path, sync::Arc};

fn runtime(workspace: &Path, home: &Path, enabled: bool) -> Runtime {
    Runtime::load_with_environment(
        workspace,
        RuntimeOverrides {
            settings: Settings {
                resources: ResourceSettings {
                    enabled: Setting::Value(false),
                    global: Setting::Value(false),
                    ..Default::default()
                },
                packages: PackageSettings {
                    enabled: Setting::Value(enabled),
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
fn install(runtime: &Runtime, source: &Path, executable: bool, marker: &Path) -> PackageRecord {
    fs::write(
        source.join("colors.json"),
        r##"{"base":"dark","accent":"#123456"}"##,
    )
    .unwrap();
    fs::write(source.join("other.json"), r##"{"accent":"#654321"}"##).unwrap();
    let mut value = json!({"schema_version":1,"id":"palette","version":"1","files":["colors.json","other.json"],
        "resources":{"themes":["colors.json"]},"permissions":{}});
    if executable {
        value["files"]
            .as_array_mut()
            .unwrap()
            .push(json!("program"));
        value["permissions"]["execution"] = json!(true);
        value["executables"] = json!([{"id":"never","protocol":"process","program":{"kind":"bundled","path":"program"}}]);
        fs::write(
            source.join("program"),
            format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(source.join("program"), fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::write(
        source.join(MANIFEST_FILE),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    runtime
        .package_store()
        .unwrap()
        .install_local(
            source,
            InstallOptions {
                enable: true,
                allow_execution: executable,
            },
            &TurnExecutionContext::new(),
        )
        .unwrap()
}

#[test]
fn explicit_theme_assets_work_without_discovery_or_global_enablement_and_never_activate_code() {
    let source = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let runtime = runtime(workspace.path(), home.path(), true);
    let marker = workspace.path().join("must-not-run");
    let record = install(&runtime, source.path(), true, &marker);
    runtime
        .package_store()
        .unwrap()
        .enable("palette", false)
        .unwrap();
    let asset = runtime
        .package_theme_asset("palette", "colors.json", 4096)
        .unwrap();
    assert_eq!(asset.pin(), &record.pin);
    assert_eq!(asset.path(), "colors.json");
    assert_eq!(
        asset.bytes(),
        fs::read(source.path().join("colors.json")).unwrap()
    );
    assert!(!format!("{asset:?}").contains("#123456"));
    assert!(!marker.exists());
    assert!(runtime.extensions().is_none());
}

#[test]
fn theme_loading_enforces_declared_kind_bounds_paths_and_host_policy() {
    let source = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let host = runtime(workspace.path(), home.path(), true);
    install(
        &host,
        source.path(),
        false,
        &workspace.path().join("unused"),
    );
    for path in ["other.json", "../colors.json", "/colors.json", ".env"] {
        assert!(host.package_theme_asset("palette", path, 4096).is_err());
    }
    assert!(
        host.package_theme_asset("palette", "colors.json", 4)
            .is_err()
    );
    assert!(
        host.package_theme_asset("palette", "colors.json", 32 * 1024 * 1024 + 1)
            .is_err()
    );
    assert!(
        host.package_theme_asset("unknown", "colors.json", 4096)
            .is_err()
    );
    let disabled = runtime(workspace.path(), home.path(), false);
    assert!(
        disabled
            .package_theme_asset("palette", "colors.json", 4096)
            .is_err()
    );
}

#[test]
fn changed_or_removed_packages_cannot_change_a_captured_asset_or_restore_trust() {
    let source = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let host = runtime(workspace.path(), home.path(), true);
    install(
        &host,
        source.path(),
        false,
        &workspace.path().join("unused"),
    );
    let asset = host
        .package_theme_asset("palette", "colors.json", 4096)
        .unwrap();
    let original = asset.bytes().to_vec();
    let store = host.package_store().unwrap();
    let snapshot = store.snapshot(Some(&["palette".into()]), None).unwrap();
    fs::write(
        snapshot.get("palette").unwrap().root().join("colors.json"),
        r##"{"accent":"#000000"}"##,
    )
    .unwrap();
    assert!(
        host.package_theme_asset("palette", "colors.json", 4096)
            .is_err()
    );
    assert_eq!(asset.bytes(), original);
    store.uninstall("palette").unwrap();
    assert!(
        host.package_theme_asset("palette", "colors.json", 4096)
            .is_err()
    );
    assert_eq!(asset.bytes(), original);
}

#[test]
fn missing_theme_storage_is_not_created_by_a_read() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let host = runtime(workspace.path(), home.path(), true);
    assert!(
        host.package_theme_asset("palette", "colors.json", 4096)
            .is_err()
    );
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
}
