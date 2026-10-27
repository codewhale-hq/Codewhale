use super::*;
use std::sync::Mutex;

pub(super) type MockForm = Vec<(String, String)>;
pub(super) type MockPost = (String, MockForm);
pub(super) struct SiwcSigningFixture {
    pair: ring::signature::Ed25519KeyPair,
    pub(super) keys: JwkSet,
}

impl SiwcSigningFixture {
    pub(super) fn new() -> Self {
        use ring::signature::KeyPair as _;
        let pkcs8 =
            ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
                .unwrap();
        let pair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let keys = serde_json::from_value(serde_json::json!({"keys": [{
            "kty": "OKP", "crv": "Ed25519", "kid": "siwc-fixture", "alg": "EdDSA", "use": "sig",
            "x": URL_SAFE_NO_PAD.encode(pair.public_key().as_ref())
        }]}))
        .unwrap();
        Self { pair, keys }
    }
    pub(super) fn token(&self, claims: Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"EdDSA","kid":"siwc-fixture"}"#);
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let message = format!("{header}.{payload}");
        format!(
            "{message}.{}",
            URL_SAFE_NO_PAD.encode(self.pair.sign(message.as_bytes()).as_ref())
        )
    }
    #[cfg(test)]
    pub(super) fn id_claims(&self) -> Value {
        serde_json::json!({"iss": CHATGPT_OAUTH_ISSUER, "aud": "oaiapp_codewhale_test", "sub": "test-sub", "nonce": "pending-nonce", "exp": now_unix_secs().unwrap() + 3600})
    }
    pub(super) fn access(&self, subject: &str) -> String {
        self.token(serde_json::json!({"iss": CHATGPT_OAUTH_ISSUER, "aud": CHATGPT_OAUTH_RESOURCE, "sub": subject, "client_id": "oaiapp_codewhale_test", "scope": CHATGPT_OAUTH_SCOPE, "exp": now_unix_secs().unwrap() + 3600}))
    }
}

struct SiwcRefreshClient {
    pub(super) keys: JwkSet,
    access: String,
    posts: Mutex<Vec<MockPost>>,
}
impl OAuthFormClient for SiwcRefreshClient {
    fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<(u16, String)> {
        self.posts.lock().unwrap().push((
            url.to_string(),
            form.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        ));
        Ok((200, serde_json::json!({"access_token": self.access, "refresh_token": "rotated-refresh", "expires_in": 3600, "scope": CHATGPT_OAUTH_SCOPE, "token_type": "Bearer"}).to_string()))
    }
    fn chatgpt_jwks(&self, _: &str) -> Result<JwkSet> {
        Ok(self.keys.clone())
    }
}

pub fn siwc_refresh_rotates_atomically_and_rejects_a_different_account<
    T: PartialEq + std::fmt::Debug,
>(
    scope_of: impl Fn(&Config) -> T,
) {
    let _lock = crate::test_support::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let root = home.path().canonicalize().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &root);
    let signer = SiwcSigningFixture::new();
    let mut config = Config::default();
    config.provider = Some("openai-codex".into());
    install_test_chatgpt_registration(&mut config).unwrap();
    let original = get_owned_credentials_read_only(OAuthProvider::Chatgpt, &config).unwrap();
    let original_scope = scope_of(&config);
    let path = configured_owned_auth_file_path(OAuthProvider::Chatgpt, &config)
        .unwrap()
        .unwrap();
    let name = path.file_name().unwrap().to_str().unwrap();
    codewhale_config::with_xai_oauth_lifecycle_lock(|store| {
        let mut file = load_owned_auth_file_from_store(store, name)?.unwrap();
        for entry in file.values_mut() {
            entry.expires_at = Some("2000-01-01T00:00:00Z".to_string());
        }
        write_auth_file_to_store(store, name, &file, true)
    })
    .unwrap();
    let before = std::fs::read(&path).unwrap();
    let wrong = SiwcRefreshClient {
        keys: signer.keys.clone(),
        access: signer.access("another-account"),
        posts: Mutex::new(Vec::new()),
    };
    assert!(get_owned_credentials_with(OAuthProvider::Chatgpt, &config, &wrong).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let right = SiwcRefreshClient {
        keys: signer.keys.clone(),
        access: signer.access("test-sub"),
        posts: Mutex::new(Vec::new()),
    };
    let refreshed = get_owned_credentials_with(OAuthProvider::Chatgpt, &config, &right).unwrap();
    assert_ne!(original.access_token, refreshed.access_token);
    assert_eq!(original_scope, scope_of(&config));
    assert_eq!(refreshed.refresh_token.as_deref(), Some("rotated-refresh"));
    assert_eq!(
        official_chatgpt_registration(&config).unwrap().subject,
        "test-sub"
    );
    assert!(get_owned_credentials_read_only(OAuthProvider::Chatgpt, &config).is_ok());
    let posts = right.posts.lock().unwrap();
    assert_eq!(
        posts[0].0,
        "https://auth.openai.com/api/accounts/oauth/token"
    );
    assert!(
        posts[0]
            .1
            .contains(&("client_id".to_string(), "oaiapp_codewhale_test".to_string()))
    );
    assert!(
        posts[0]
            .1
            .contains(&("resource".to_string(), CHATGPT_OAUTH_RESOURCE.to_string()))
    );
    assert!(
        posts[0]
            .1
            .contains(&("refresh_token".to_string(), "siwc-test-refresh".to_string()))
    );
}

pub fn descriptor() -> PluginOAuthConfig {
    PluginOAuthConfig {
        issuer: "https://issuer.example".into(),
        authorization_endpoint: "https://issuer.example/authorize".into(),
        token_endpoint: "https://issuer.example/token".into(),
        client_id: "public-client".into(),
        scopes: vec!["models:invoke".into()],
        resource: Some("https://api.example/oauth".into()),
        callback_path: plugin_callback_path(),
    }
}
