use aicoder_core::{
    model::ModelToolCall,
    tools::{AllowAllApproval, DispatcherConfig, ExecutableTool, ToolContext, ToolDispatcher},
};
use aicoder_runtime::tools::{
    GlobTool, GrepTool, ListDirectoryTool, ReadFileTool, default_registry,
};
use serde_json::{Value, json};
use std::{fs, io::Write, path::Path, sync::Arc, time::Duration};

fn context(root: &Path, bytes: usize) -> ToolContext {
    ToolContext::new(root, Duration::from_secs(10), bytes).unwrap()
}
fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

#[tokio::test]
async fn utf8_and_json_escaped_long_lines_resume_without_gaps_or_lost_characters() {
    let root = tempfile::tempdir().unwrap();
    let original = "\"\\\t中文🦀".repeat(800);
    write(root.path(), "long.txt", &original);
    let ctx = context(root.path(), 1024);
    let mut args = json!({"path":"long.txt"});
    let mut reconstructed = String::new();
    let mut last_offset = 0;
    let mut pages = 0;
    loop {
        let page = ReadFileTool.execute(&ctx, args.clone()).await.unwrap();
        assert!(page.output.to_string().len() <= 1024);
        assert_eq!(page.output["offset_line"], 1);
        reconstructed.push_str(page.output["content"].as_str().unwrap());
        pages += 1;
        if page.output["has_more"] == false {
            break;
        }
        assert!(page.truncated);
        assert_eq!(page.output["content_truncated"], true);
        assert_eq!(page.output["next_offset_line"], 1);
        let offset = page.output["next_offset_byte"].as_u64().unwrap();
        assert!(offset > last_offset);
        last_offset = offset;
        args["cursor"] = page.output["next_cursor"].clone();
        assert!(pages < 200);
    }
    assert!(pages > 2);
    assert_eq!(reconstructed, original);
}

#[tokio::test]
async fn read_pages_handle_crlf_blank_lines_and_final_lines_without_newlines() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "file", "first\r\n\r\nlast");
    let ctx = context(root.path(), 4096);
    let mut args = json!({"path":"file","limit_lines":1});
    for (index, expected) in ["first", "", "last"].into_iter().enumerate() {
        let page = ReadFileTool
            .execute(&ctx, args.clone())
            .await
            .unwrap()
            .output;
        assert_eq!(page["content"], expected);
        assert_eq!(page["offset_line"], index + 1);
        assert_eq!(page["lines_read"], 1);
        assert_eq!(page["end_line_complete"], true);
        assert_eq!(page["has_more"], index < 2);
        args["cursor"] = page["next_cursor"].clone();
    }
    write(root.path(), "bad", "x");
    fs::write(root.path().join("bad"), [b'x', 0xf0, 0x9f]).unwrap();
    assert_eq!(
        ReadFileTool
            .execute(&ctx, json!({"path":"bad"}))
            .await
            .unwrap_err()
            .code,
        "non_utf8"
    );
}

#[tokio::test]
async fn file_cursors_reject_changes_query_mixups_and_invalid_control_fields() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "file", &"line\n".repeat(400));
    let ctx = context(root.path(), 1024);
    let page = ReadFileTool
        .execute(&ctx, json!({"path":"file","limit_lines":1}))
        .await
        .unwrap()
        .output;
    let cursor = page["next_cursor"].clone();
    assert_eq!(
        ReadFileTool
            .execute(&ctx, json!({"path":"file", "offset_line":u64::MAX}))
            .await
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    assert_eq!(
        ReadFileTool
            .execute(&ctx, json!({"path":"file","offset_line":1,"cursor":cursor}))
            .await
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    let mut forged: Value = serde_json::from_str(cursor.as_str().unwrap()).unwrap();
    forged["line"] = json!(u64::MAX);
    assert_eq!(
        ReadFileTool
            .execute(&ctx, json!({"path":"file","cursor":forged.to_string()}))
            .await
            .unwrap_err()
            .code,
        "invalid_cursor"
    );
    write(root.path(), "file", "changed\n");
    assert_eq!(
        ReadFileTool
            .execute(&ctx, json!({"path":"file","cursor":cursor}))
            .await
            .unwrap_err()
            .code,
        "stale_cursor"
    );
}

#[tokio::test]
async fn glob_and_listing_pages_are_sorted_and_honor_nested_gitignore_and_visibility() {
    let root = tempfile::tempdir().unwrap();
    for name in [
        "z.rs",
        "a.rs",
        "src/nested/b.rs",
        "src/c.py",
        "src/drop.rs",
        "ignored/x.rs",
        ".hidden/h.rs",
    ] {
        write(root.path(), name, "content");
    }
    write(
        root.path(),
        ".gitignore",
        "ignored/\n*.rs\n!a.rs\n!z.rs\n!src/\n",
    );
    write(root.path(), "src/.gitignore", "!*.rs\ndrop.rs\n");
    let ctx = context(root.path(), 2048);
    let mut args = json!({"pattern":"*.rs","limit":1});
    let mut found = Vec::new();
    loop {
        let page = GlobTool.execute(&ctx, args.clone()).await.unwrap().output;
        found.extend(
            page["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["path"].as_str().unwrap().to_string()),
        );
        if page["has_more"] == false {
            break;
        }
        args["cursor"] = page["next_cursor"].clone();
    }
    assert_eq!(found, ["a.rs", "src/nested/b.rs", "z.rs"]);
    let page = GlobTool
        .execute(
            &ctx,
            json!({"path":"src","pattern":"**/*.{rs,py}","respect_gitignore":false}),
        )
        .await
        .unwrap()
        .output;
    assert_eq!(page["entries"].as_array().unwrap().len(), 3);
    let page = GlobTool
        .execute(
            &ctx,
            json!({"pattern":"*.rs","respect_gitignore":false,"hidden":true}),
        )
        .await
        .unwrap()
        .output;
    assert_eq!(page["entries"].as_array().unwrap().len(), 6);
    let first = ListDirectoryTool
        .execute(&ctx, json!({"limit":1,"respect_gitignore":false}))
        .await
        .unwrap()
        .output;
    assert_eq!(first["entries"][0]["path"], "a.rs");
    let second = ListDirectoryTool
        .execute(
            &ctx,
            json!({"limit":1,"respect_gitignore":false,"cursor":first["next_cursor"]}),
        )
        .await
        .unwrap()
        .output;
    assert_eq!(second["entries"][0]["path"], "ignored");
}

#[tokio::test]
async fn grep_pages_keep_all_matching_lines_across_files_and_expose_read_cursors() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "b.txt", "hit one\nhit two");
    write(root.path(), "a.txt", "other\nHIT three\n");
    write(root.path(), "ignored.log", "hit hidden");
    write(root.path(), ".gitignore", "*.log\n");
    let ctx = context(root.path(), 4096);
    let mut args = json!({"pattern":"hit","case_insensitive":true,"max_results":1});
    let mut results = Vec::new();
    loop {
        let page = GrepTool.execute(&ctx, args.clone()).await.unwrap().output;
        assert!(page.to_string().len() <= 4096);
        for record in page["records"].as_array().unwrap() {
            results.push((
                record["path"].as_str().unwrap().to_string(),
                record["line"].as_u64().unwrap(),
            ));
            let read = ReadFileTool
                .execute(
                    &ctx,
                    json!({"path":record["path"],"cursor":record["read_cursor"],"limit_lines":1}),
                )
                .await
                .unwrap()
                .output;
            assert_eq!(read["content"], record["text"]);
        }
        if page["has_more"] == false {
            break;
        }
        args["cursor"] = page["next_cursor"].clone();
        assert!(results.len() < 10);
    }
    assert_eq!(
        results,
        [
            ("a.txt".into(), 2),
            ("b.txt".into(), 1),
            ("b.txt".into(), 2)
        ]
    );
}

#[tokio::test]
async fn grep_output_budget_preserves_cursor_and_does_not_drop_unreturned_matches() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "file",
        &format!("hit {}\n", "\"\\".repeat(400)).repeat(12),
    );
    let ctx = context(root.path(), 4096);
    let mut args = json!({"pattern":"hit"});
    let mut lines = Vec::new();
    loop {
        let page = GrepTool.execute(&ctx, args.clone()).await.unwrap().output;
        assert!(page.to_string().len() <= 4096);
        lines.extend(
            page["records"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["line"].as_u64().unwrap()),
        );
        if page["has_more"] == false {
            break;
        }
        args["cursor"] = page["next_cursor"].clone();
        assert!(lines.len() < 20);
    }
    assert_eq!(lines, (1..=12).collect::<Vec<_>>());
}

#[tokio::test]
async fn grep_can_continue_after_a_scan_budget_with_no_matches_in_the_first_page() {
    let root = tempfile::tempdir().unwrap();
    let mut file = fs::File::create(root.path().join("large.txt")).unwrap();
    let line = format!("{}\n", "x".repeat(60_000));
    for _ in 0..290 {
        file.write_all(line.as_bytes()).unwrap();
    }
    file.write_all(b"needle\n").unwrap();
    drop(file);
    let ctx = context(root.path(), 4096);
    let first = GrepTool
        .execute(&ctx, json!({"pattern":"needle"}))
        .await
        .unwrap()
        .output;
    assert_eq!(first["records"], json!([]));
    assert_eq!(first["has_more"], true);
    assert!(first["scan_bytes"].as_u64().unwrap() <= 16 * 1024 * 1024);
    let second = GrepTool
        .execute(
            &ctx,
            json!({"pattern":"needle","cursor":first["next_cursor"]}),
        )
        .await
        .unwrap()
        .output;
    assert_eq!(second["records"][0]["line"], 291);
    assert_eq!(second["has_more"], false);
    assert_eq!(
        ReadFileTool
            .execute(&ctx, json!({"path":"large.txt","offset_line":291}))
            .await
            .unwrap_err()
            .code,
        "scan_limit"
    );
    let record = &second["records"][0];
    let read = ReadFileTool
        .execute(
            &ctx,
            json!({"path":record["path"],"cursor":record["read_cursor"],"limit_lines":1}),
        )
        .await
        .unwrap()
        .output;
    assert_eq!(read["content"], "needle");
}

#[tokio::test]
async fn oversized_search_lines_return_a_readable_recovery_cursor_and_binary_files_are_counted() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "long", &"a".repeat(1024 * 1024));
    let ctx = context(root.path(), 4096);
    let error = GrepTool
        .execute(&ctx, json!({"pattern":"a"}))
        .await
        .unwrap_err();
    assert_eq!(error.code, "line_too_long");
    let details = error.details.unwrap();
    let page = ReadFileTool
        .execute(
            &ctx,
            json!({"path":details["path"],"cursor":details["read_cursor"]}),
        )
        .await
        .unwrap()
        .output;
    assert_eq!(page["offset_line"], 1);
    assert_eq!(page["content_truncated"], true);
    fs::write(root.path().join("binary"), [0; 128 * 1024]).unwrap();
    let page = GrepTool
        .execute(&ctx, json!({"pattern":"a","path":"binary"}))
        .await
        .unwrap()
        .output;
    assert_eq!(page["binary_files_skipped"], 1);
    assert_eq!(page["has_more"], false);
}

#[tokio::test]
async fn bounded_scope_ignore_files_and_invalid_patterns_fail_explicitly() {
    let root = tempfile::tempdir().unwrap();
    let ctx = context(root.path(), 4096);
    assert_eq!(
        GlobTool
            .execute(&ctx, json!({"pattern":"["}))
            .await
            .unwrap_err()
            .code,
        "invalid_glob"
    );
    assert_eq!(
        GrepTool
            .execute(&ctx, json!({"pattern":"["}))
            .await
            .unwrap_err()
            .code,
        "invalid_regex"
    );
    assert_eq!(
        ReadFileTool
            .execute(&context(root.path(), 5), json!({"path":"missing"}))
            .await
            .unwrap_err()
            .code,
        "output_limit_too_small"
    );
    write(root.path(), ".gitignore", &"a".repeat(65537));
    assert_eq!(
        GlobTool
            .execute(&ctx, json!({"pattern":"*"}))
            .await
            .unwrap_err()
            .code,
        "scope_limit"
    );
    fs::remove_file(root.path().join(".gitignore")).unwrap();
    let mut directory = root.path().to_path_buf();
    for _ in 0..65 {
        directory.push("a");
        fs::create_dir(&directory).unwrap();
    }
    assert_eq!(
        GlobTool
            .execute(&ctx, json!({"pattern":"*"}))
            .await
            .unwrap_err()
            .code,
        "scope_limit"
    );
    let page = GlobTool
        .execute(&ctx, json!({"path":directory,"pattern":"*"}))
        .await
        .unwrap()
        .output;
    assert_eq!(page["has_more"], false);
}

#[cfg(unix)]
#[tokio::test]
async fn enumerators_do_not_follow_symlinks_and_special_files_cannot_block_readers() {
    use std::{
        ffi::CString,
        os::unix::{ffi::OsStrExt, fs::symlink},
    };
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "secret", "needle outside");
    symlink(outside.path(), root.path().join("alias")).unwrap();
    let fifo = root.path().join("pipe");
    let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let ctx = context(root.path(), 4096);
    let page = ListDirectoryTool
        .execute(&ctx, json!({}))
        .await
        .unwrap()
        .output;
    assert_eq!(page["entries"][0]["kind"], "symlink");
    assert_eq!(page["entries"][1]["kind"], "other");
    assert_eq!(
        GlobTool
            .execute(&ctx, json!({"pattern":"*"}))
            .await
            .unwrap()
            .output["entries"],
        json!([])
    );
    assert_eq!(
        GrepTool
            .execute(&ctx, json!({"pattern":"needle"}))
            .await
            .unwrap()
            .output["records"],
        json!([])
    );
    assert_eq!(
        ReadFileTool
            .execute(&ctx, json!({"path":"alias/secret"}))
            .await
            .unwrap_err()
            .code,
        "path_outside_workspace"
    );
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        ReadFileTool.execute(&ctx, json!({"path":"pipe"})),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error.code, "not_a_file");
}

#[tokio::test]
async fn dispatched_pages_remain_structured_and_usable_under_the_tool_output_cap() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "file", &"needle\n".repeat(1000));
    let dispatcher = ToolDispatcher::new(
        Arc::new(default_registry().unwrap()),
        root.path(),
        Arc::new(AllowAllApproval),
        DispatcherConfig {
            max_output_bytes: 2048,
            ..Default::default()
        },
    )
    .unwrap();
    let mut arguments = json!({"pattern":"needle"});
    let mut seen = Vec::new();
    loop {
        let messages = dispatcher
            .dispatch(&[ModelToolCall {
                id: "call".into(),
                name: "grep".into(),
                arguments: arguments.clone(),
            }])
            .await
            .unwrap();
        let (_, envelope, _) = messages[0].tool_result().unwrap();
        assert_eq!(envelope["ok"], true, "{envelope}");
        let output = &envelope["output"];
        assert!(output.is_object());
        seen.extend(
            output["records"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["line"].as_u64().unwrap()),
        );
        if output["has_more"] == false {
            break;
        }
        arguments["cursor"] = output["next_cursor"].clone();
        assert!(seen.len() <= 1000);
    }
    assert_eq!(seen, (1..=1000).collect::<Vec<_>>());
}

#[tokio::test]
async fn search_cursors_are_scoped_to_patterns_visibility_and_unchanged_files() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "a.rs", "hit one\nhit two\n");
    write(root.path(), "b.rs", "hit three\n");
    let ctx = context(root.path(), 4096);
    let page = GrepTool
        .execute(&ctx, json!({"pattern":"hit","max_results":1}))
        .await
        .unwrap()
        .output;
    for args in [
        json!({"pattern":"other","cursor":page["next_cursor"]}),
        json!({"pattern":"hit","hidden":true,"cursor":page["next_cursor"]}),
    ] {
        assert_eq!(
            GrepTool.execute(&ctx, args).await.unwrap_err().code,
            "invalid_cursor"
        );
    }
    write(root.path(), "a.rs", "changed");
    assert_eq!(
        GrepTool
            .execute(&ctx, json!({"pattern":"hit","cursor":page["next_cursor"]}))
            .await
            .unwrap_err()
            .code,
        "stale_cursor"
    );
    let page = GlobTool
        .execute(&ctx, json!({"pattern":"*.rs","limit":1}))
        .await
        .unwrap()
        .output;
    assert_eq!(
        GlobTool
            .execute(
                &ctx,
                json!({"pattern":"*.txt","cursor":page["next_cursor"]})
            )
            .await
            .unwrap_err()
            .code,
        "invalid_cursor"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn atomic_replacement_invalidates_file_cursors_even_with_the_same_length_and_timestamp() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "file", "first\nsecond");
    let ctx = context(root.path(), 4096);
    let page = ReadFileTool
        .execute(&ctx, json!({"path":"file","limit_lines":1}))
        .await
        .unwrap()
        .output;
    let time = fs::metadata(root.path().join("file"))
        .unwrap()
        .modified()
        .unwrap();
    write(root.path(), "replacement", "other\nsecond");
    fs::File::open(root.path().join("replacement"))
        .unwrap()
        .set_modified(time)
        .unwrap();
    fs::rename(root.path().join("replacement"), root.path().join("file")).unwrap();
    assert_eq!(
        ReadFileTool
            .execute(&ctx, json!({"path":"file","cursor":page["next_cursor"]}))
            .await
            .unwrap_err()
            .code,
        "stale_cursor"
    );
}

#[tokio::test]
async fn binary_detection_accounts_for_discarded_readahead_across_many_files() {
    let root = tempfile::tempdir().unwrap();
    let mut bytes = vec![b'x'; 8192];
    bytes[0] = 0xff;
    bytes[1] = b'\n';
    for index in 0..2100 {
        fs::write(root.path().join(format!("{index:04}")), &bytes).unwrap();
    }
    let ctx = context(root.path(), 4096);
    let first = GrepTool
        .execute(&ctx, json!({"pattern":"needle"}))
        .await
        .unwrap()
        .output;
    assert_eq!(first["has_more"], true);
    assert_eq!(first["binary_files_skipped"], 2048);
    assert_eq!(first["scan_bytes"], 16 * 1024 * 1024);
    let second = GrepTool
        .execute(
            &ctx,
            json!({"pattern":"needle","cursor":first["next_cursor"]}),
        )
        .await
        .unwrap()
        .output;
    assert_eq!(second["has_more"], false);
    assert_eq!(second["binary_files_skipped"], 52);
}
