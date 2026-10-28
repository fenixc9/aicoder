use aicoder_core::{model::*, session::*};
use serde_json::json;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    process::Command,
};
fn user(text: &str) -> Message {
    Message::text(MessageRole::User, text)
}

#[test]
fn v2_read_is_unchanged_and_explicit_migration_keeps_ids_and_an_exact_backup() {
    let workspace = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let repository = JsonlSessionRepository::new(root.path()).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let path = root.path().join("legacy-v2.jsonl");
    let original = format!(
        "{}\n{}\n",
        json!({"type":"session","version":2,"id":id,"created_at":123,"cwd":workspace.path()}),
        json!({"type":"message","id":"old-entry","created_at":124,"message":user("remember")})
    );
    fs::write(&path, &original).unwrap();
    let mut session = repository.open(&id).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    assert_eq!(session.metadata().version, 2);
    let backup = repository.migrate(&mut session).unwrap().unwrap();
    assert_eq!(fs::read_to_string(backup).unwrap(), original);
    assert_eq!(session.metadata().id, id);
    assert_eq!(session.metadata().created_at, 123);
    assert_eq!(session.metadata().updated_at, 124);
    assert_eq!(session.messages()[0].id, "old-entry");
    assert_eq!(session.messages()[0].created_at, 124);
    assert_eq!(repository.list(workspace.path()).unwrap().len(), 1);
    assert!(repository.migrate(&mut session).unwrap().is_none());
    assert_eq!(
        repository.open(&id).unwrap().messages()[0].message,
        &user("remember")
    );
}

#[test]
fn torn_batch_is_discarded_as_a_whole_and_the_next_write_repairs_the_tail() {
    let workspace = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let repository = JsonlSessionRepository::new(root.path()).unwrap();
    let mut session = repository.create(workspace.path()).unwrap();
    repository.append(&mut session, user("retained")).unwrap();
    let length = fs::metadata(session.path().unwrap()).unwrap().len();
    repository
        .append_all(&mut session, [user("lost one"), user("lost two")])
        .unwrap();
    let path = session.path().unwrap();
    let file = OpenOptions::new().write(true).open(path).unwrap();
    file.set_len(fs::metadata(path).unwrap().len() - 10)
        .unwrap();
    let mut recovered = repository.open(&session.metadata().id).unwrap();
    assert_eq!(recovered.chat_messages(), vec![user("retained")]);
    repository.append(&mut recovered, user("new")).unwrap();
    assert!(fs::metadata(path).unwrap().len() > length);
    assert_eq!(
        repository
            .open(&session.metadata().id)
            .unwrap()
            .chat_messages(),
        vec![user("retained"), user("new")]
    );
}

#[test]
fn checksum_detects_valid_json_corruption_and_same_length_edits_cause_conflicts() {
    let workspace = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let repository = JsonlSessionRepository::new(root.path()).unwrap();
    let mut session = repository.create(workspace.path()).unwrap();
    repository.append(&mut session, user("original")).unwrap();
    let path = session.path().unwrap().to_owned();
    let old = fs::read_to_string(&path).unwrap();
    fs::write(&path, old.replace("original", "tampered")).unwrap();
    assert!(
        repository
            .open(&session.metadata().id)
            .unwrap_err()
            .to_string()
            .contains("checksum")
    );
    assert!(
        repository
            .append(&mut session, user("no"))
            .unwrap_err()
            .downcast_ref::<SessionConflict>()
            .is_some()
    );
    assert_eq!(session.chat_messages(), vec![user("original")]);
}

#[test]
fn complete_invalid_last_record_and_duplicate_checksum_fields_are_not_torn_tails() {
    let workspace = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let repository = JsonlSessionRepository::new(root.path()).unwrap();
    let mut session = repository.create(workspace.path()).unwrap();
    repository.append(&mut session, user("message")).unwrap();
    let path = session.path().unwrap();
    let original = fs::read_to_string(path).unwrap();
    OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(b"{\"type\":\"unknown\"}")
        .unwrap();
    assert!(repository.open(&session.metadata().id).is_err());
    // Duplicate data after the checksum must not evade the raw-byte envelope validation.
    let edited = original.trim_end().strip_suffix('}').unwrap().to_owned() + ",\"entries\":[]}\n";
    fs::write(path, edited).unwrap();
    assert!(repository.open(&session.metadata().id).is_err());
}

#[test]
fn process_crash_fixture() {
    let Ok(root) = std::env::var("AICODER_CRASH_FIXTURE_ROOT") else {
        return;
    };
    let id = std::env::var("AICODER_CRASH_FIXTURE_ID").unwrap();
    let repository = JsonlSessionRepository::new(root).unwrap();
    let mut session = repository.open(&id).unwrap();
    let _lease = repository.acquire_run(&mut session).unwrap();
    repository
        .append_entries(
            &mut session,
            vec![
                SessionEntryData::TurnStart {
                    turn_id: "crash".into(),
                },
                SessionEntryData::Message {
                    message: user("before process exit"),
                },
            ],
        )
        .unwrap();
    // Exit deliberately bypasses Rust destructors, leaving the run's owner token on disk.
    std::process::exit(0);
}
#[test]
fn process_exit_releases_the_os_run_lock_without_a_terminal_record() {
    let workspace = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let repository = JsonlSessionRepository::new(root.path()).unwrap();
    let session = repository.create(workspace.path()).unwrap();
    let id = session.metadata().id.clone();
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "process_crash_fixture", "--nocapture"])
        .env("AICODER_CRASH_FIXTURE_ROOT", root.path())
        .env("AICODER_CRASH_FIXTURE_ID", &id)
        .status()
        .unwrap();
    assert!(status.success());
    let mut recovered = repository.open(&id).unwrap();
    assert_eq!(recovered.unfinished_turn(), Some("crash"));
    assert_eq!(
        recovered.messages()[0].message,
        &user("before process exit")
    );
    let lease = repository.acquire_run(&mut recovered).unwrap();
    drop(lease);
}

#[cfg(unix)]
#[test]
fn files_and_backups_are_private_and_lock_symlinks_are_rejected() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let workspace = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let repository = JsonlSessionRepository::new(root.path()).unwrap();
    let session = repository.create(workspace.path()).unwrap();
    assert_eq!(
        fs::metadata(root.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(session.path().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let lock = root
        .path()
        .join(".runs")
        .join(format!("{}.lock", session.metadata().id));
    let target = root.path().join("target");
    fs::write(&target, "do not touch").unwrap();
    symlink(&target, &lock).unwrap();
    let mut session = repository.open(&session.metadata().id).unwrap();
    assert!(repository.acquire_run(&mut session).is_err());
    assert_eq!(fs::read_to_string(target).unwrap(), "do not touch");
}
