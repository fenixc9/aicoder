#![cfg(feature = "package-downloads")]
use aicoder_core::TurnExecutionContext;
use aicoder_runtime::packages::*;
use base64::Engine;
use serde_json::json;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

struct Request {
    path: String,
    method: String,
    body: Vec<u8>,
    headers: BTreeMap<String, String>,
}
struct Response {
    code: u16,
    body: Vec<u8>,
    headers: Vec<(String, String)>,
    delay: Duration,
}
impl Response {
    fn ok(body: impl Into<Vec<u8>>) -> Self {
        Self {
            code: 200,
            body: body.into(),
            headers: Vec::new(),
            delay: Duration::ZERO,
        }
    }
}
struct Server {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new(handler: impl Fn(&str, Request) -> Response + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let base = url.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let records = requests.clone();
        let worker = thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let Some(request) = read_request(&mut stream) else {
                            continue;
                        };
                        records.lock().unwrap().push(request.path.clone());
                        let response = handler(&base, request);
                        thread::sleep(response.delay);
                        let mut head = format!(
                            "HTTP/1.1 {} fixture\r\nContent-Length: {}\r\nConnection: close\r\n",
                            response.code,
                            response.body.len()
                        );
                        for (name, value) in response.headers {
                            head.push_str(&format!("{name}: {value}\r\n"));
                        }
                        head.push_str("\r\n");
                        let _ = stream.write_all(head.as_bytes());
                        let _ = stream.write_all(&response.body);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            url,
            requests,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut bytes = Vec::new();
    let mut one = [0u8; 1];
    while !bytes.ends_with(b"\r\n\r\n") {
        if bytes.len() >= 8192 || stream.read(&mut one).ok()? == 0 {
            return None;
        }
        bytes.push(one[0]);
    }
    let text = std::str::from_utf8(&bytes).ok()?;
    let mut lines = text.split("\r\n");
    let mut line = lines.next()?.split_whitespace();
    let method = line.next()?.to_owned();
    let path = line.next()?.to_owned();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(a, b)| (a.to_lowercase(), b.trim().to_owned()))
        .collect::<BTreeMap<_, _>>();
    let length = headers
        .get("content-length")
        .map_or(Some(0), |v| v.parse::<usize>().ok())?;
    if length > 1024 * 1024 {
        return None;
    }
    let mut body = vec![0; length];
    stream.read_exact(&mut body).ok()?;
    Some(Request {
        path,
        method,
        body,
        headers,
    })
}
fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
    let gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gzip);
    for (name, bytes) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o600);
        header.set_cksum();
        tar.append_data(&mut header, *name, *bytes).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap()
}
fn fixture_archive() -> Vec<u8> {
    let manifest = json!({"schema_version":1,"id":"downloaded","version":"1.0.0","files":["rules.md"],"permissions":{"instructions":true},"resources":{"instructions":["rules.md"]}});
    archive(&[("package/aicoder-package.json",&serde_json::to_vec(&manifest).unwrap()),
        ("package/package.json",br#"{"name":"@scope/example","version":"1.0.0","scripts":{"postinstall":"must-not-run"}}"#),
        ("package/rules.md",b"downloaded rules")])
}
fn metadata(base: &str, bytes: &[u8]) -> Vec<u8> {
    serde_json::to_vec(&json!({"name":"@scope/example","version":"1.0.0","dist":{"tarball":format!("{base}/archive"),
        "integrity":format!("sha512-{}",base64::engine::general_purpose::STANDARD.encode(ring::digest::digest(&ring::digest::SHA512,bytes).as_ref()))}})).unwrap()
}
#[derive(Clone)]
struct Bundle {
    pin: PackagePin,
    bytes: Vec<u8>,
    manifest: Vec<u8>,
    content: String,
}
fn bundle(
    reference: &PackageStore,
    id: &str,
    dependencies: Vec<PackagePin>,
    content: &str,
) -> Bundle {
    let manifest=serde_json::to_vec(&json!({"schema_version":1,"id":id,"version":"1.0.0","files":["rules.md"],
        "permissions":{"instructions":true},"resources":{"instructions":["rules.md"]},"dependencies":dependencies})).unwrap();
    let package =
        serde_json::to_vec(&json!({"name":format!("@scope/{id}"),"version":"1.0.0"})).unwrap();
    let bytes = archive(&[
        ("package/aicoder-package.json", &manifest),
        ("package/package.json", &package),
        ("package/rules.md", content.as_bytes()),
    ]);
    let source = tempfile::tempdir().unwrap();
    std::fs::write(source.path().join(MANIFEST_FILE), &manifest).unwrap();
    std::fs::write(source.path().join("rules.md"), content).unwrap();
    let pin = reference
        .install_local(
            source.path(),
            InstallOptions {
                enable: false,
                allow_execution: false,
            },
            &TurnExecutionContext::new(),
        )
        .unwrap()
        .pin;
    Bundle {
        pin,
        bytes,
        manifest,
        content: content.into(),
    }
}
fn bundle_server(bundles: Vec<Bundle>) -> Server {
    Server::new(move |base, request| bundle_response(base, request, &bundles))
}
fn bundle_response(base: &str, request: Request, bundles: &[Bundle]) -> Response {
    for bundle in bundles {
        if request.path == format!("/archive/{}", bundle.pin.id) {
            return Response::ok(bundle.bytes.clone());
        }
        if request.path.contains(&format!("%2F{}", bundle.pin.id)) {
            let mut value: serde_json::Value =
                serde_json::from_slice(&metadata(base, &bundle.bytes)).unwrap();
            value["name"] = json!(format!("@scope/{}", bundle.pin.id));
            value["dist"]["tarball"] = json!(format!("{base}/archive/{}", bundle.pin.id));
            return Response::ok(serde_json::to_vec(&value).unwrap());
        }
    }
    Response {
        code: 404,
        body: Vec::new(),
        headers: Vec::new(),
        delay: Duration::ZERO,
    }
}
fn install_bundle(store: &PackageStore, bundle: &Bundle) {
    let source = tempfile::tempdir().unwrap();
    std::fs::write(source.path().join(MANIFEST_FILE), &bundle.manifest).unwrap();
    std::fs::write(source.path().join("rules.md"), &bundle.content).unwrap();
    store
        .install_local(
            source.path(),
            InstallOptions {
                enable: false,
                allow_execution: false,
            },
            &TurnExecutionContext::new(),
        )
        .unwrap();
}
fn request(server: &Server, id: &str) -> PackageRequest {
    PackageRequest::Npm {
        registry: server.url.clone(),
        package: format!("@scope/{id}"),
        version: None,
    }
}
fn catalogue(server: &Server, bundles: &[Bundle]) -> PackageSourceCatalogue {
    PackageSourceCatalogue {
        schema_version: 1,
        entries: bundles
            .iter()
            .map(|b| PackageSourceEntry {
                pin: b.pin.clone(),
                request: request(server, &b.pin.id),
            })
            .collect(),
    }
}

#[tokio::test]
async fn transitive_sources_publish_one_complete_graph_without_enabling_dependency_roots() {
    let reference = tempfile::tempdir().unwrap();
    let reference = PackageStore::open(reference.path()).unwrap();
    let leaf = bundle(&reference, "leaf", vec![], "leaf");
    let middle = bundle(&reference, "middle", vec![leaf.pin.clone()], "middle");
    let root = bundle(&reference, "root", vec![middle.pin.clone()], "root");
    let server = bundle_server(vec![leaf.clone(), middle.clone(), root.clone()]);
    let home = tempfile::tempdir().unwrap();
    let store = PackageStore::open(home.path()).unwrap();
    let record = client()
        .install_with_sources(
            &store,
            &request(&server, "root"),
            &catalogue(&server, &[middle, leaf]),
            InstallOptions::default(),
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert_eq!(record.pin, root.pin);
    let index = store.index().unwrap();
    assert_eq!(index.revision, 1);
    assert_eq!(index.installed.len(), 3);
    assert_eq!(
        index.enabled.iter().cloned().collect::<Vec<_>>(),
        vec!["root"]
    );
    assert_eq!(
        store
            .snapshot(None, None)
            .unwrap()
            .packages()
            .iter()
            .map(|p| p.pin.id.as_str())
            .collect::<Vec<_>>(),
        vec!["leaf", "middle", "root"]
    );
    assert_eq!(server.requests.lock().unwrap().len(), 6);
    assert_eq!(
        std::fs::read_dir(home.path().join("staging"))
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn mismatched_or_missing_dependency_sources_never_publish_a_partial_graph() {
    for missing in [false, true] {
        let reference = tempfile::tempdir().unwrap();
        let reference = PackageStore::open(reference.path()).unwrap();
        let leaf = bundle(&reference, "leaf", vec![], "expected");
        let root = bundle(&reference, "root", vec![leaf.pin.clone()], "root");
        let wrong = bundle(&reference, "leaf", vec![], "different content");
        let server = bundle_server(vec![wrong, root]);
        let home = tempfile::tempdir().unwrap();
        let store = PackageStore::open(home.path()).unwrap();
        let sources = if missing {
            PackageSourceCatalogue::default()
        } else {
            catalogue(&server, &[leaf])
        };
        assert!(
            client()
                .install_with_sources(
                    &store,
                    &request(&server, "root"),
                    &sources,
                    InstallOptions::default(),
                    TurnExecutionContext::new()
                )
                .await
                .is_err()
        );
        assert_eq!(store.index().unwrap().revision, 0);
        assert!(store.index().unwrap().installed.is_empty());
        assert_eq!(
            std::fs::read_dir(home.path().join("staging"))
                .unwrap()
                .count(),
            0
        );
    }
}

#[tokio::test]
async fn aggregate_graph_budget_rejects_before_publication() {
    let reference = tempfile::tempdir().unwrap();
    let reference = PackageStore::open(reference.path()).unwrap();
    let leaf = bundle(&reference, "leaf", vec![], "leaf");
    let root = bundle(&reference, "root", vec![leaf.pin.clone()], "root");
    let server = bundle_server(vec![leaf.clone(), root]);
    let home = tempfile::tempdir().unwrap();
    let store = PackageStore::open(home.path()).unwrap();
    let mut limits = PackageAcquireLimits::default();
    limits.max_graph_bytes = 6000;
    let acquirer = PackageAcquirer::new(limits)
        .unwrap()
        .with_loopback_http(true);
    assert!(
        acquirer
            .install_with_sources(
                &store,
                &request(&server, "root"),
                &catalogue(&server, &[leaf]),
                InstallOptions::default(),
                TurnExecutionContext::new()
            )
            .await
            .is_err()
    );
    assert!(store.index().unwrap().installed.is_empty());
    assert_eq!(store.index().unwrap().revision, 0);
}
#[tokio::test]
async fn concurrent_registry_edits_reject_prepared_graph_without_rolling_back_the_other_owner() {
    let reference = tempfile::tempdir().unwrap();
    let reference = PackageStore::open(reference.path()).unwrap();
    let stable = bundle(&reference, "stable", vec![], "stable");
    let root = bundle(&reference, "root", vec![], "root");
    let home = tempfile::tempdir().unwrap();
    let store = Arc::new(PackageStore::open(home.path()).unwrap());
    install_bundle(&store, &stable);
    let writer = store.clone();
    let changed = AtomicBool::new(false);
    let server = Server::new(move |base, request| {
        if !changed.swap(true, Ordering::SeqCst) {
            writer.enable("stable", false).unwrap();
        }
        bundle_response(base, request, std::slice::from_ref(&root))
    });
    let before = store.index().unwrap().revision;
    let error = client()
        .install(
            &store,
            &request(&server, "root"),
            InstallOptions::default(),
            TurnExecutionContext::new(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("registry changed"));
    assert_eq!(store.index().unwrap().revision, before + 1);
    assert_eq!(store.index().unwrap().installed.len(), 1);
    assert_eq!(
        store
            .snapshot(Some(&["stable".into()]), None)
            .unwrap()
            .get("stable")
            .unwrap()
            .pin,
        stable.pin
    );
    assert_eq!(
        std::fs::read_dir(home.path().join("staging"))
            .unwrap()
            .count(),
        0
    );
}
#[tokio::test]
async fn source_catalogue_cannot_route_credentials_or_admit_unbounded_or_duplicate_pins() {
    let root = tempfile::tempdir().unwrap();
    let reference = PackageStore::open(root.path().join("reference")).unwrap();
    let leaf = bundle(&reference, "leaf", vec![], "leaf");
    let server = bundle_server(vec![leaf.clone()]);
    let mut sources = catalogue(&server, std::slice::from_ref(&leaf));
    sources.entries.push(sources.entries[0].clone());
    let home = tempfile::tempdir().unwrap();
    let store = PackageStore::open(home.path()).unwrap();
    assert!(
        client()
            .install_with_sources(
                &store,
                &request(&server, "leaf"),
                &sources,
                InstallOptions::default(),
                TurnExecutionContext::new()
            )
            .await
            .is_err()
    );
    assert!(server.requests.lock().unwrap().is_empty());
    let path = root.path().join("sources.json");
    let mut value = serde_json::to_value(catalogue(&server, &[leaf])).unwrap();
    value["entries"][0]["token_env"] = json!("MUST_NOT_ROUTE");
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(PackageSourceCatalogue::read(&path).is_err());
    std::fs::write(&path, vec![b' '; 256 * 1024 + 1]).unwrap();
    assert!(PackageSourceCatalogue::read(&path).is_err());
}
#[tokio::test]
async fn cancellation_after_root_preparation_discards_the_whole_dependency_transaction() {
    let reference = tempfile::tempdir().unwrap();
    let reference = PackageStore::open(reference.path()).unwrap();
    let leaf = bundle(&reference, "leaf", vec![], "leaf");
    let root = bundle(&reference, "root", vec![leaf.pin.clone()], "root");
    let server = Server::new(move |base, request| {
        let delay = request.path.contains("%2Fleaf");
        let mut response = bundle_response(base, request, &[leaf.clone(), root.clone()]);
        if delay {
            response.delay = Duration::from_millis(300);
        }
        response
    });
    let leaf = reference
        .snapshot(Some(&["leaf".into()]), None)
        .unwrap()
        .get("leaf")
        .unwrap()
        .pin
        .clone();
    let sources = PackageSourceCatalogue {
        schema_version: 1,
        entries: vec![PackageSourceEntry {
            pin: leaf,
            request: request(&server, "leaf"),
        }],
    };
    let home = tempfile::tempdir().unwrap();
    let store = PackageStore::open(home.path()).unwrap();
    let context = TurnExecutionContext::new();
    let cancel = context.clone();
    let requests = server.requests.clone();
    let watcher = tokio::spawn(async move {
        for _ in 0..200 {
            if requests
                .lock()
                .unwrap()
                .iter()
                .any(|p| p.contains("%2Fleaf"))
            {
                cancel.cancel("cancel dependency");
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("dependency did not start");
    });
    assert!(
        client()
            .install_with_sources(
                &store,
                &request(&server, "root"),
                &sources,
                InstallOptions::default(),
                context
            )
            .await
            .is_err()
    );
    watcher.await.unwrap();
    assert!(store.index().unwrap().installed.is_empty());
    assert_eq!(
        std::fs::read_dir(home.path().join("staging"))
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn reused_dependency_does_not_upgrade_global_active_selection_or_fetch_again() {
    let home = tempfile::tempdir().unwrap();
    let store = PackageStore::open(home.path()).unwrap();
    let old = bundle(&store, "leaf", vec![], "old");
    let newer = bundle(&store, "leaf", vec![], "new");
    let root = bundle(&store, "root", vec![old.pin.clone()], "root");
    // Keep the root source out of installed state so this is a real root acquisition.
    store.uninstall("root").unwrap();
    let server = bundle_server(vec![root]);
    client()
        .install(
            &store,
            &request(&server, "root"),
            InstallOptions::default(),
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert_eq!(store.index().unwrap().active["leaf"], newer.pin.sha256);
    assert_eq!(
        store.snapshot(None, None).unwrap().get("leaf").unwrap().pin,
        old.pin
    );
    assert_eq!(server.requests.lock().unwrap().len(), 2);
}
fn client() -> PackageAcquirer {
    PackageAcquirer::new(PackageAcquireLimits::default())
        .unwrap()
        .with_loopback_http(true)
}
fn npm(server: &Server) -> PackageRequest {
    PackageRequest::Npm {
        registry: server.url.clone(),
        package: "@scope/example".into(),
        version: None,
    }
}

#[tokio::test]
async fn npm_resolves_a_tag_verifies_bytes_and_publishes_only_declared_native_payload() {
    let bytes = fixture_archive();
    let server = Server::new(move |base, request| {
        Response::ok(if request.path == "/archive" {
            bytes.clone()
        } else {
            metadata(base, &bytes)
        })
    });
    let home = tempfile::tempdir().unwrap();
    let store = PackageStore::open(home.path()).unwrap();
    let record = client()
        .install(
            &store,
            &npm(&server),
            InstallOptions::default(),
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert_eq!(record.pin.version, "1.0.0");
    assert_eq!(store.index().unwrap().version, 2);
    assert!(matches!(record.source,PackageSource::Npm {version,..} if version=="1.0.0"));
    let snapshot = store.snapshot(None, None).unwrap();
    let object = snapshot.get("downloaded").unwrap();
    assert_eq!(
        object.read_asset("rules.md", 1024).unwrap(),
        b"downloaded rules"
    );
    assert!(object.read_asset("package.json", 1024).is_err());
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    assert!(server.requests.lock().unwrap()[0].contains("%2F"));
    assert_eq!(
        std::fs::read_dir(home.path().join("staging"))
            .unwrap()
            .count(),
        0
    );
    let path = home.path().join("registry.json");
    let original = std::fs::read(&path).unwrap();
    let mut forged: serde_json::Value = serde_json::from_slice(&original).unwrap();
    forged["installed"][&record.pin.sha256]["source"]["registry"] =
        json!("https://user:private-value@example.invalid/");
    std::fs::write(&path, serde_json::to_vec(&forged).unwrap()).unwrap();
    let error = store.index().unwrap_err();
    assert!(!format!("{error:#}").contains("private-value"));
    std::fs::write(&path, original).unwrap();
}

#[tokio::test]
async fn bad_integrity_identity_redirect_and_origin_do_not_replace_registry() {
    for variant in ["integrity", "identity", "version", "redirect", "origin"] {
        let bytes = fixture_archive();
        let server = Server::new(move |base, request| {
            if request.path == "/archive" {
                return Response::ok(bytes.clone());
            }
            let mut value: serde_json::Value =
                serde_json::from_slice(&metadata(base, &bytes)).unwrap();
            match variant {
                "integrity" => value["dist"]["integrity"] = json!("sha512-AQID"),
                "identity" => value["name"] = json!("another"),
                "version" => value["version"] = json!("latest"),
                "origin" => value["dist"]["tarball"] = json!("http://127.0.0.1:1/foreign"),
                "redirect" => {
                    return Response {
                        code: 302,
                        body: Vec::new(),
                        headers: vec![("Location".into(), "http://127.0.0.1:1/foreign".into())],
                        delay: Duration::ZERO,
                    };
                }
                _ => unreachable!(),
            }
            Response::ok(serde_json::to_vec(&value).unwrap())
        });
        let home = tempfile::tempdir().unwrap();
        let store = PackageStore::open(home.path()).unwrap();
        let stable = tempfile::tempdir().unwrap();
        std::fs::write(stable.path().join(MANIFEST_FILE),serde_json::to_vec(&json!({"schema_version":1,"id":"stable","version":"1","files":[],"resources":{},"permissions":{}})).unwrap()).unwrap();
        let stable_record = store
            .install_local(
                stable.path(),
                InstallOptions::default(),
                &TurnExecutionContext::new(),
            )
            .unwrap();
        let before = store.index().unwrap();
        assert!(
            client()
                .install(
                    &store,
                    &npm(&server),
                    InstallOptions::default(),
                    TurnExecutionContext::new()
                )
                .await
                .is_err(),
            "{variant}"
        );
        assert_eq!(store.index().unwrap().revision, before.revision);
        assert_eq!(store.index().unwrap().installed.len(), 1);
        assert_eq!(
            store
                .snapshot(None, None)
                .unwrap()
                .get("stable")
                .unwrap()
                .pin,
            stable_record.pin
        );
        assert_eq!(
            std::fs::read_dir(home.path().join("staging"))
                .unwrap()
                .count(),
            0
        );
    }
}

#[tokio::test]
async fn total_deadline_and_cancellation_stop_without_an_install_or_retry() {
    for cancelled in [false, true] {
        let bytes = fixture_archive();
        let server = Server::new(move |base, request| {
            let mut response = Response::ok(if request.path == "/archive" {
                bytes.clone()
            } else {
                metadata(base, &bytes)
            });
            response.delay = Duration::from_millis(80);
            response
        });
        let home = tempfile::tempdir().unwrap();
        let store = PackageStore::open(home.path()).unwrap();
        let mut limits = PackageAcquireLimits::default();
        limits.timeout = Duration::from_millis(125);
        let acquirer = PackageAcquirer::new(limits)
            .unwrap()
            .with_loopback_http(true);
        let context = TurnExecutionContext::new();
        let signal = context.clone();
        let cancel = tokio::spawn(async move {
            if cancelled {
                tokio::time::sleep(Duration::from_millis(20)).await;
                signal.cancel("fixture cancel");
            }
        });
        let started = Instant::now();
        assert!(
            acquirer
                .install(&store, &npm(&server), InstallOptions::default(), context)
                .await
                .is_err()
        );
        assert!(started.elapsed() < Duration::from_millis(250));
        cancel.await.unwrap();
        assert!(store.index().unwrap().installed.is_empty());
        assert!(server.requests.lock().unwrap().len() <= 2);
    }
}
#[tokio::test]
async fn host_bearer_binding_is_origin_scoped_and_values_never_enter_errors_or_provenance() {
    let bytes = fixture_archive();
    let server = Server::new(move |base, request| {
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some("Bearer fixture-private-token")
        );
        Response::ok(if request.path == "/archive" {
            bytes.clone()
        } else {
            metadata(base, &bytes)
        })
    });
    let host = Arc::new(BTreeMap::from([(
        "FIXTURE_TOKEN".into(),
        "fixture-private-token".into(),
    )]));
    let acquirer = client()
        .with_environment(host)
        .with_bearer_reference(&server.url, "FIXTURE_TOKEN")
        .unwrap();
    let home = tempfile::tempdir().unwrap();
    let store = PackageStore::open(home.path()).unwrap();
    let record = acquirer
        .install(
            &store,
            &npm(&server),
            InstallOptions::default(),
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert!(
        !serde_json::to_string(&record)
            .unwrap()
            .contains("fixture-private-token")
    );
    assert!(
        !std::fs::read_to_string(home.path().join("registry.json"))
            .unwrap()
            .contains("fixture-private-token")
    );
    let broken = Server::new(|_, _| Response {
        code: 403,
        body: b"fixture-private-token".to_vec(),
        headers: Vec::new(),
        delay: Duration::ZERO,
    });
    let error = acquirer
        .install(
            &store,
            &npm(&broken),
            InstallOptions::default(),
            TurnExecutionContext::new(),
        )
        .await
        .unwrap_err();
    assert!(!format!("{error:#}").contains("fixture-private-token"));
}

fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    output.stdout
}
#[tokio::test]
async fn cancellation_during_git_transfer_awaits_namespace_cleanup_without_publication() {
    let server = Server::new(|_, _| Response {
        code: 500,
        body: Vec::new(),
        headers: Vec::new(),
        delay: Duration::from_millis(500),
    });
    let home = tempfile::tempdir().unwrap();
    let store = PackageStore::open(home.path()).unwrap();
    let context = TurnExecutionContext::new();
    let signal = context.clone();
    let records = server.requests.clone();
    let cancel = tokio::spawn(async move {
        for _ in 0..200 {
            if !records.lock().unwrap().is_empty() {
                signal.cancel("cancel actual Git transfer");
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("Git transfer did not reach the fixture");
    });
    assert!(
        client()
            .install(
                &store,
                &PackageRequest::Git {
                    url: format!("{}/repo.git", server.url),
                    revision: "HEAD".into()
                },
                InstallOptions::default(),
                context
            )
            .await
            .is_err()
    );
    cancel.await.unwrap();
    assert!(store.index().unwrap().installed.is_empty());
    assert_eq!(
        std::fs::read_dir(home.path().join("staging"))
            .unwrap()
            .count(),
        0
    );
}
#[tokio::test]
async fn actual_git_http_fetch_records_the_resolved_commit_without_checkout_or_scripts() {
    let source = tempfile::tempdir().unwrap();
    git(source.path(), &["init", "-q"]);
    std::fs::write(source.path().join("aicoder-package.json"),serde_json::to_vec(&json!({"schema_version":1,"id":"git-pkg","version":"1","files":["rules.md"],"resources":{"instructions":["rules.md"]},"permissions":{"instructions":true}})).unwrap()).unwrap();
    std::fs::write(source.path().join("rules.md"), "git rules").unwrap();
    git(source.path(), &["add", "."]);
    git(source.path(), &["commit", "-qm", "fixture"]);
    let commit = String::from_utf8(git(source.path(), &["rev-parse", "HEAD"]))
        .unwrap()
        .trim()
        .to_owned();
    let repos = tempfile::tempdir().unwrap();
    let bare = repos.path().join("repo.git");
    git(
        repos.path(),
        &[
            "clone",
            "--bare",
            source.path().to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    let exec = String::from_utf8(git(source.path(), &["--exec-path"])).unwrap();
    let backend = Path::new(exec.trim()).join("git-http-backend");
    let root = repos.path().to_owned();
    let server = Server::new(move |_, request| {
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some("Bearer fixture-git-token")
        );
        let (path, query) = request.path.split_once('?').unwrap_or((&request.path, ""));
        let mut child = Command::new(&backend)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_PROJECT_ROOT", &root)
            .env("GIT_HTTP_EXPORT_ALL", "1")
            .env("REQUEST_METHOD", &request.method)
            .env("PATH_INFO", path)
            .env("QUERY_STRING", query)
            .env(
                "CONTENT_TYPE",
                request
                    .headers
                    .get("content-type")
                    .map(String::as_str)
                    .unwrap_or(""),
            )
            .env("CONTENT_LENGTH", request.body.len().to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&request.body)
            .unwrap();
        let output = child.wait_with_output().unwrap();
        let offset = output
            .stdout
            .windows(4)
            .position(|v| v == b"\r\n\r\n")
            .unwrap();
        let head = String::from_utf8_lossy(&output.stdout[..offset]);
        let headers = head
            .split("\r\n")
            .filter_map(|line| line.split_once(':'))
            .map(|(a, b)| (a.to_owned(), b.trim().to_owned()))
            .collect::<Vec<_>>();
        Response {
            code: 200,
            body: output.stdout[offset + 4..].to_vec(),
            headers,
            delay: Duration::ZERO,
        }
    });
    let home = tempfile::tempdir().unwrap();
    let store = PackageStore::open(home.path()).unwrap();
    let acquirer = client()
        .with_environment(Arc::new(BTreeMap::from([(
            "GIT_TOKEN".into(),
            "fixture-git-token".into(),
        )])))
        .with_bearer_reference(&server.url, "GIT_TOKEN")
        .unwrap();
    let record = acquirer
        .install(
            &store,
            &PackageRequest::Git {
                url: format!("{}/repo.git", server.url),
                revision: commit.clone(),
            },
            InstallOptions::default(),
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert!(matches!(&record.source,PackageSource::Git {commit:actual,..} if actual==&commit));
    assert_eq!(
        store
            .snapshot(None, None)
            .unwrap()
            .get("git-pkg")
            .unwrap()
            .read_asset("rules.md", 1024)
            .unwrap(),
        b"git rules"
    );
    assert!(server.requests.lock().unwrap().len() >= 2);
    assert!(
        !serde_json::to_string(&record)
            .unwrap()
            .contains("fixture-git-token")
    );
    // Incompressible object data exceeds the private fetch filesystem even though it is
    // not a declared resource. It cannot fill host storage before package allowlist import.
    let mut random = vec![0u8; 20 * 1024 * 1024];
    let mut state = 1234567u32;
    for byte in &mut random {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        *byte = state as u8;
    }
    std::fs::write(source.path().join("huge.bin"), random).unwrap();
    git(source.path(), &["add", "huge.bin"]);
    git(source.path(), &["commit", "-qm", "large object"]);
    git(
        source.path(),
        &["push", bare.to_str().unwrap(), "HEAD:refs/heads/large"],
    );
    let mut limits = PackageAcquireLimits::default();
    limits.git_disk_bytes = 16 * 1024 * 1024;
    let constrained = PackageAcquirer::new(limits)
        .unwrap()
        .with_loopback_http(true)
        .with_environment(Arc::new(BTreeMap::from([(
            "GIT_TOKEN".into(),
            "fixture-git-token".into(),
        )])))
        .with_bearer_reference(&server.url, "GIT_TOKEN")
        .unwrap();
    let before = store.index().unwrap().revision;
    assert!(
        constrained
            .install(
                &store,
                &PackageRequest::Git {
                    url: format!("{}/repo.git", server.url),
                    revision: "large".into()
                },
                InstallOptions::default(),
                TurnExecutionContext::new()
            )
            .await
            .is_err()
    );
    assert_eq!(store.index().unwrap().revision, before);
}
