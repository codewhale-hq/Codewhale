use codewhale_app_server_protocol::rpc::{
    RequestId, RpcCall, RpcErrorDetails, RpcErrorResponse, RpcMessage, RpcResponse,
};
use serde_json::{Value, json};

#[test]
fn correlated_rpc_envelopes_match_pinned_stable_schema() {
    let request_schema: Value =
        serde_json::from_str(include_str!("pinned/JSONRPCRequest.json")).unwrap();
    let response_schema: Value =
        serde_json::from_str(include_str!("pinned/JSONRPCResponse.json")).unwrap();
    let error_schema: Value =
        serde_json::from_str(include_str!("pinned/JSONRPCError.json")).unwrap();
    let message_schema: Value =
        serde_json::from_str(include_str!("pinned/JSONRPCMessage.json")).unwrap();
    let notification_schema: Value =
        serde_json::from_str(include_str!("pinned/JSONRPCNotification.json")).unwrap();
    let message_validator = jsonschema::validator_for(&message_schema).unwrap();
    let notification_validator = jsonschema::validator_for(&notification_schema).unwrap();
    let request_validator = jsonschema::validator_for(&request_schema).unwrap();
    let response_validator = jsonschema::validator_for(&response_schema).unwrap();
    let error_validator = jsonschema::validator_for(&error_schema).unwrap();
    for id in [
        RequestId::String(String::new()),
        RequestId::String("request/string".into()),
        RequestId::Integer(i64::MIN),
        RequestId::Integer(i64::MAX),
    ] {
        let call: RpcCall = serde_json::from_value(json!({"id":id,"method":"initialize","params":{"clientInfo":{"name":"fixture","version":"1"}}})).unwrap();
        let request = serde_json::to_value(call).unwrap();
        assert!(request.get("jsonrpc").is_none());
        assert!(request_validator.is_valid(&request));
        assert!(message_validator.is_valid(&request));
        assert!(matches!(
            serde_json::from_value::<RpcMessage>(request).unwrap(),
            RpcMessage::Request(_)
        ));
        let response = serde_json::to_value(RpcResponse {
            id: id.clone(),
            result: json!({}),
        })
        .unwrap();
        assert!(response_validator.is_valid(&response));
        assert!(message_validator.is_valid(&response));
        assert!(matches!(
            serde_json::from_value::<RpcMessage>(response).unwrap(),
            RpcMessage::Response(_)
        ));
        let error = serde_json::to_value(RpcErrorResponse {
            id,
            error: RpcErrorDetails {
                code: -32600,
                data: None,
                message: "Not initialized".into(),
            },
        })
        .unwrap();
        assert!(error_validator.is_valid(&error));
        assert!(error["error"].get("data").is_none());
        assert!(message_validator.is_valid(&error));
        assert!(matches!(
            serde_json::from_value::<RpcMessage>(error).unwrap(),
            RpcMessage::Error(_)
        ));
    }
    let notification: RpcMessage = serde_json::from_value(json!({"method":"initialized"})).unwrap();
    let value = serde_json::to_value(notification).unwrap();
    assert!(notification_validator.is_valid(&value));
    assert!(message_validator.is_valid(&value));
    assert!(value.get("id").is_none());
}

#[test]
fn rpc_call_preserves_notification_absence_and_refuses_non_ids() {
    let notification: RpcCall =
        serde_json::from_value(json!({"jsonrpc":"2.0","method":"initialized"})).unwrap();
    let value = serde_json::to_value(notification).unwrap();
    assert!(value.get("id").is_none());
    assert!(value.get("jsonrpc").is_none());
    for id in [
        json!(null),
        json!(true),
        json!(1.0),
        json!(u64::MAX),
        json!({}),
        json!([]),
    ] {
        assert!(serde_json::from_value::<RequestId>(id.clone()).is_err());
        let call: RpcCall = serde_json::from_value(json!({"id":id,"method":"shutdown"})).unwrap();
        assert!(call.id.is_none());
    }
    for version in [json!(null), json!(true), json!("1.0")] {
        let call: RpcCall =
            serde_json::from_value(json!({"jsonrpc":version,"id":1,"method":"initialize"}))
                .unwrap();
        assert_eq!(call.id, Some(RequestId::Integer(1)));
        assert!(serde_json::to_value(call).unwrap().get("jsonrpc").is_none());
    }
    assert!(serde_json::from_value::<RpcCall>(json!({"id":1,"method":7})).is_err());
    assert!(serde_json::from_value::<RpcMessage>(json!({"id":null,"result":{}})).is_err());
    assert!(
        serde_json::from_value::<RpcMessage>(
            json!({"id":null,"error":{"code":-32600,"message":"invalid"}})
        )
        .is_err()
    );
    assert!(serde_json::from_value::<RpcCall>(json!({"id":1,"result":{}})).is_err());
    for params in [json!({}), json!(null), json!(["value"])] {
        let message: RpcMessage =
            serde_json::from_value(json!({"id":1,"method":"fixture","params":params})).unwrap();
        let encoded = serde_json::to_value(message).unwrap();
        if params.is_null() {
            assert!(encoded.get("params").is_none());
        } else {
            assert_eq!(encoded["params"], params);
        }
    }
}

#[test]
fn initialization_contract_matches_pinned_stable_schema() {
    use codewhale_app_server_protocol::lifecycle::{InitializeParams, InitializeResponse};
    let params_schema: Value =
        serde_json::from_str(include_str!("pinned/InitializeParams.json")).unwrap();
    let result_schema: Value =
        serde_json::from_str(include_str!("pinned/InitializeResponse.json")).unwrap();
    let params: InitializeParams = serde_json::from_value(json!({"clientInfo":{"name":"fixture","version":"1"},"capabilities":{"experimentalApi":false,"optOutNotificationMethods":null}})).unwrap();
    assert!(
        jsonschema::validator_for(&params_schema)
            .unwrap()
            .is_valid(&serde_json::to_value(params).unwrap())
    );
    let result = InitializeResponse {
        user_agent: "Codewhale/fixture".into(),
        codex_home: std::path::PathBuf::from("/fixture/codewhale"),
        platform_family: "unix".into(),
        platform_os: "macos".into(),
    };
    let value = serde_json::to_value(result).unwrap();
    assert!(
        jsonschema::validator_for(&result_schema)
            .unwrap()
            .is_valid(&value)
    );
    assert_eq!(value["codexHome"], "/fixture/codewhale");
}
