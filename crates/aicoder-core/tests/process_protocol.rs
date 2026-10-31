use aicoder_core::extensions::process_protocol::*;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
fn matches<T: Serialize + DeserializeOwned>(value: Value) {
    let decoded: T = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), value);
}
#[test]
fn published_wire_fixture_matches_the_public_cross_language_contract() {
    let mut methods = std::collections::HashMap::new();
    for line in include_str!("../../../docs/protocol/process-tools-v1.jsonl").lines() {
        let mut frame: Value = serde_json::from_str(line).unwrap();
        assert_eq!(frame["jsonrpc"], "2.0");
        if let Some(method) = frame["method"].as_str().map(str::to_owned) {
            match method.as_str() {
                INITIALIZE => {
                    assert_eq!(frame["params"]["protocolVersion"], PROTOCOL_VERSION);
                    matches::<Initialize>(frame["params"].take());
                }
                PREPARE | CALL => matches::<Invocation>(frame["params"].take()),
                COMMIT | DISPOSE => matches::<Plan>(frame["params"].take()),
                _ => panic!("unknown published request"),
            }
            methods.insert(frame["id"].as_u64().unwrap(), method);
        } else {
            let method = methods.remove(&frame["id"].as_u64().unwrap()).unwrap();
            if frame.get("error").is_some() {
                continue;
            }
            match method.as_str() {
                INITIALIZE => matches::<Initialized>(frame["result"].take()),
                PREPARE => matches::<Prepared>(frame["result"].take()),
                CALL | COMMIT => matches::<Completed>(frame["result"].take()),
                DISPOSE => matches::<Disposed>(frame["result"].take()),
                _ => unreachable!(),
            }
        }
    }
    assert!(methods.is_empty());
}
