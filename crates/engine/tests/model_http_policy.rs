use std::sync::{Arc, Mutex};

use anyhow::{Result, ensure};
use codewhale_engine::client::CodewhaleClient;
use codewhale_engine::llm_client::LlmClient;
use codewhale_engine::tools::spec::{ToolContext, ToolSpec};
use codewhale_engine::vision::tools::ImageAnalyzeTool;
use codewhale_runtime::config::{
    Config, ModelHttpPolicy, ProviderConfig, ProvidersConfig, VisionModelConfig,
};
use serde_json::{Value, json};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Debug)]
struct Policy {
    rejected_base_url: Option<String>,
    urls: Mutex<Vec<String>>,
}

impl ModelHttpPolicy for Policy {
    fn configure(
        &self,
        builder: reqwest::ClientBuilder,
        base_url: &str,
    ) -> Result<reqwest::ClientBuilder> {
        self.urls.lock().unwrap().push(base_url.to_owned());
        if self.rejected_base_url.as_deref() == Some(base_url) {
            return Err(anyhow::anyhow!("probe host rejected").context("probe policy refusal"));
        }
        Ok(builder.no_proxy().user_agent("model-http-policy-probe"))
    }
}

fn config(base_url: &str, policy: Option<Arc<dyn ModelHttpPolicy>>) -> Config {
    let mut config = Config::default();
    config.provider = Some("deepseek".into());
    config.providers = Some(ProvidersConfig {
        deepseek: ProviderConfig {
            api_key: Some("local-probe-key".into()),
            base_url: Some(base_url.into()),
            model: Some("deepseek-v4-pro".into()),
            ..Default::default()
        },
        ..Default::default()
    });
    config.model_http_policy = policy;
    config.vision_model = Some(VisionModelConfig {
        model_http_policy: None,
        model: "probe-vision".into(),
        api_key: Some("local-probe-key".into()),
        base_url: Some(base_url.into()),
    });
    config
}

fn request(text: &str) -> codewhale_models::MessageRequest {
    serde_json::from_value(json!({
        "model": "deepseek-v4-pro",
        "messages": [{"role": "user", "content": [{"type": "text", "text": text}]}],
        "max_tokens": 16,
        "stream": false
    }))
    .unwrap()
}

#[tokio::test]
async fn model_http_policy_reaches_primary_and_vision_transports() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-policy-probe",
            "object": "chat.completion",
            "model": "deepseek-v4-pro",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "probe ok"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let workspace = tempfile::tempdir()?;
    tokio::fs::write(
        workspace.path().join("probe.png"),
        include_bytes!("fixtures/ocr_hello.png"),
    )
    .await?;
    let context = ToolContext::new(workspace.path());

    let rejecting = Arc::new(Policy {
        rejected_base_url: Some(server.uri()),
        urls: Mutex::default(),
    });
    let rejected = config(&server.uri(), Some(rejecting.clone()));
    let error = match CodewhaleClient::new(&rejected) {
        Ok(_) => anyhow::bail!("primary accepted rejected host"),
        Err(error) => format!("{error:#}"),
    };
    ensure!(error.contains("probe policy refusal") && error.contains("probe host rejected"));
    ensure!(rejecting.urls.lock().unwrap().len() == 1);
    println!("PASS primary rejection: {error}");

    let tool =
        ImageAnalyzeTool::new_with_route_client(rejected.vision_model_config().unwrap(), None);
    let error = match tool
        .execute(json!({"image_path": "probe.png"}), &context)
        .await
    {
        Ok(_) => anyhow::bail!("vision accepted rejected host"),
        Err(error) => error.to_string(),
    };
    ensure!(error.contains("probe policy refusal") && error.contains("probe host rejected"));
    ensure!(rejecting.urls.lock().unwrap().len() == 2);
    ensure!(server.received_requests().await.unwrap().is_empty());
    println!("PASS vision rejection: {error}; requests=0");

    let mutating = Arc::new(Policy {
        rejected_base_url: None,
        urls: Mutex::default(),
    });
    let configured = config(&server.uri(), Some(mutating.clone())).clone();
    let client = CodewhaleClient::new(&configured)?;
    client.create_message(request("configured policy")).await?;
    let requests = server.received_requests().await.unwrap();
    ensure!(requests.len() == 1);
    ensure!(requests[0].headers.get("user-agent").unwrap() == "model-http-policy-probe");
    ensure!(mutating.urls.lock().unwrap().len() == 3);
    println!(
        "PASS primary builder mutation: user-agent=model-http-policy-probe; requests=1; builders=3"
    );

    client
        .http1_fallback_client()
        .get(format!("{}/fallback", server.uri()))
        .send()
        .await?
        .error_for_status()?;
    let requests = server.received_requests().await.unwrap();
    ensure!(requests.len() == 2);
    ensure!(requests[1].headers.get("user-agent").unwrap() == "model-http-policy-probe");
    println!("PASS HTTP1 builder mutation: user-agent=model-http-policy-probe; requests=1");

    let tool =
        ImageAnalyzeTool::new_with_route_client(configured.vision_model_config().unwrap(), None);
    let result = tool
        .execute(json!({"image_path": "probe.png"}), &context)
        .await?;
    let result: Value = serde_json::from_str(&result.content)?;
    ensure!(result["analysis"] == "probe ok");
    let requests = server.received_requests().await.unwrap();
    ensure!(requests.len() == 3);
    ensure!(requests[2].headers.get("user-agent").unwrap() == "model-http-policy-probe");
    ensure!(
        mutating
            .urls
            .lock()
            .unwrap()
            .iter()
            .all(|url| url == &server.uri())
    );
    ensure!(mutating.urls.lock().unwrap().len() == 4);
    println!(
        "PASS vision builder mutation: inherited policy; user-agent=model-http-policy-probe; requests=1"
    );

    let plain = config(&server.uri(), None);
    CodewhaleClient::new(&plain)?
        .create_message(request("absent policy"))
        .await?;
    let tool = ImageAnalyzeTool::new_with_route_client(plain.vision_model_config().unwrap(), None);
    ensure!(
        tool.execute(json!({"image_path": "probe.png"}), &context)
            .await?
            .success
    );
    ensure!(server.received_requests().await.unwrap().len() == 5);
    println!("PASS absent policy: primary and vision requests succeed");
    println!("execution evidence: 6 scenarios passed; 0 failed; localhost only");
    Ok(())
}
