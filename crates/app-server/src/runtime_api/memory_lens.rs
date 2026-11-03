//! Merged inside the existing Runtime API auth layer. No new listener, token,
//! CORS policy, or client-constructed capability grant is introduced here.
use super::*;
use codewhale_memory::{
    Access, Capability, Draft, Evidence, MemoryBackend, Scope, SourceKind, Status,
    ValidationReceipt,
};
use codewhale_runtime::native_memory::NativeMemoryStore;

type LensError = (StatusCode, Json<Value>);
fn fail(error: codewhale_memory::Error) -> LensError {
    let status = match &error {
        codewhale_memory::Error::Denied => StatusCode::FORBIDDEN,
        codewhale_memory::Error::NotFound => StatusCode::NOT_FOUND,
        codewhale_memory::Error::RevisionConflict
        | codewhale_memory::Error::IdempotencyConflict
        | codewhale_memory::Error::KeyConflict => StatusCode::CONFLICT,
        codewhale_memory::Error::Disabled => StatusCode::SERVICE_UNAVAILABLE,
        codewhale_memory::Error::Sql(_) | codewhale_memory::Error::Io(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
        _ => StatusCode::UNPROCESSABLE_ENTITY,
    };
    (status, Json(json!({"error":error.code()})))
}
fn internal() -> LensError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error":"memory_unavailable"})),
    )
}
fn invalid() -> LensError {
    fail(codewhale_memory::Error::Invalid("invalid request".into()))
}
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct LensQuery {
    thread_id: Option<String>,
    after: Option<String>,
    limit: Option<usize>,
}
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct EventQuery {
    after: Option<i64>,
    limit: Option<usize>,
}
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Approve {
        id: String,
        revision: i64,
        #[serde(default)]
        validation: Option<ValidationReceipt>,
    },
    Reject {
        id: String,
        revision: i64,
    },
    Forget {
        id: String,
        revision: i64,
    },
    Preferences {
        id: String,
        revision: i64,
        pinned: bool,
        suppressed: bool,
    },
    Remember {
        request_key: String,
        title: String,
        body: String,
        #[serde(default)]
        global: bool,
    },
    Replace {
        id: String,
        revision: i64,
        request_key: String,
        title: String,
        body: String,
        #[serde(default)]
        validation: Option<ValidationReceipt>,
    },
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionRequest {
    thread_id: Option<String>,
    command: Action,
}
fn valid_revision(revision: i64) -> Result<(), LensError> {
    if !(0..i64::MAX).contains(&revision) {
        return Err(invalid());
    }
    Ok(())
}
// This object is created exclusively by the authenticated server, not deserialized.
struct BoundMemory {
    store: codewhale_memory::Store,
    access: Access,
    workspace_scope: Option<Scope>,
    snapshot: codewhale_memory::Snapshot,
}
fn bind(state: &RuntimeApiState, thread_id: Option<&str>) -> Result<BoundMemory, LensError> {
    let anchor = {
        let config = state.config.read();
        if !config.memory_enabled() {
            return Err(fail(codewhale_memory::Error::Disabled));
        }
        config.memory_path()
    };
    let native = NativeMemoryStore::from_memory_anchor(&anchor);
    let workspace_id = NativeMemoryStore::workspace_id(&state.workspace).map_err(|_| internal())?;
    let owner = NativeMemoryStore::owner_scope();
    let mut scopes = vec![owner.clone()];
    let workspace_scope = workspace_id
        .as_deref()
        .map(NativeMemoryStore::workspace_scope)
        .transpose()
        .map_err(|_| internal())?;
    if let Some(scope) = &workspace_scope {
        scopes.push(scope.clone());
    }
    if let Some(id) = thread_id {
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-.:".contains(&b))
        {
            return Err(invalid());
        }
        // The server token already authorizes this local runtime. This only
        // narrows reads; the request cannot specify tenant/user/workspace grants.
        scopes.push(workspace_scope.as_ref().unwrap_or(&owner).session(id));
    }
    let access = Access::operator(scopes).map_err(fail)?;
    let store = native.open_structured().map_err(|_| internal())?;
    let snapshot = codewhale_memory::workspace::snapshot(
        &state.workspace,
        store.dependency_paths(&access).map_err(fail)?,
    )
    .map_err(fail)?;
    Ok(BoundMemory {
        store,
        access,
        workspace_scope,
        snapshot,
    })
}
pub(super) fn routes() -> Router<RuntimeApiState> {
    Router::new()
        .route("/v1/memory/lens", get(read_lens))
        .route("/v1/memory/lens/actions", post(act))
        .route("/v1/memory/events", get(events))
}
async fn read_lens(
    State(state): State<RuntimeApiState>,
    Query(query): Query<LensQuery>,
) -> Result<Json<Value>, LensError> {
    tokio::task::spawn_blocking(move || {
        let b = bind(&state, query.thread_id.as_deref())?;
        let snapshot = b
            .store
            .lens_snapshot(
                &b.access,
                query.thread_id.as_deref(),
                query.after.as_deref(),
                query.limit.unwrap_or(100),
                &b.snapshot,
            )
            .map_err(fail)?;
        Ok(Json(
            serde_json::to_value(snapshot).map_err(|_| internal())?,
        ))
    })
    .await
    .map_err(|_| internal())?
}
async fn events(
    State(state): State<RuntimeApiState>,
    Query(query): Query<EventQuery>,
) -> Result<Json<Value>, LensError> {
    tokio::task::spawn_blocking(move || {
        let b = bind(&state, None)?;
        let page = b
            .store
            .event_page(
                &b.access,
                query.after.unwrap_or(0),
                query.limit.unwrap_or(200),
            )
            .map_err(fail)?;
        Ok(Json(serde_json::to_value(page).map_err(|_| internal())?))
    })
    .await
    .map_err(|_| internal())?
}
async fn act(
    State(state): State<RuntimeApiState>,
    Json(request): Json<ActionRequest>,
) -> Result<Json<Value>, LensError> {
    tokio::task::spawn_blocking(move||{
        let mut b=bind(&state,request.thread_id.as_deref())?;
        // Mutations here are operator controls. No dispatch acknowledgement is
        // exposed over HTTP; only the engine can attest its own history/transport.
        b.access.require(Capability::Review).map_err(fail)?;
        match request.command {
            Action::Approve{id,revision,validation}=>{
                valid_revision(revision)?;
                let m=b.store.get(&b.access,&id).map_err(fail)?;
                if !(m.status==Status::Active&&m.revision==revision+1) {b.store.approve(&b.access,&id,revision,validation.as_ref(),&b.snapshot).map_err(fail)?;}
            },
            Action::Reject{id,revision}=>{
                valid_revision(revision)?;
                let m=b.store.get(&b.access,&id).map_err(fail)?;
                if !(m.status==Status::Rejected&&m.revision==revision+1) {b.store.reject(&b.access,&id,revision).map_err(fail)?;}
            },
            Action::Forget{id,revision}=>{
                valid_revision(revision)?;
                match b.store.forget(&b.access,&id,revision) {
                    Ok(_)|Err(codewhale_memory::Error::NotFound)=>{},
                    Err(error)=>return Err(fail(error)),
                }
            },
            Action::Preferences{id,revision,pinned,suppressed}=>{valid_revision(revision)?;b.store.set_preferences(&b.access,&id,revision,pinned,suppressed).map_err(fail)?;},
            Action::Remember{request_key,title,body,global}=>{
                let scope=if global {NativeMemoryStore::owner_scope()} else {b.workspace_scope.ok_or_else(invalid)?};
                // Explicit human operation: candidate capture followed by trusted
                // review. observation=0 makes retry identity stable; created_at is
                // the actual local receipt time, not an invented source timestamp.
                let draft=Draft::note(scope,title,body,Evidence{kind:SourceKind::User,uri:"codewhale:context-lens".into(),locator:"Explicit remember action".into(),sha256:None,observed_at:0});
                let receipt=b.store.capture(&b.access,&request_key,draft).map_err(fail)?;
                if receipt.memory.status==Status::Candidate {b.store.approve(&b.access,&receipt.memory.id,receipt.memory.revision,None,&b.snapshot).map_err(fail)?;}
            },
            Action::Replace{id,revision,request_key,title,body,validation}=>{
                valid_revision(revision)?;
                let old=b.store.get(&b.access,&id).map_err(fail)?;
                let mut draft=old.draft.clone();draft.title=title;draft.body=body;
                draft.evidence=vec![Evidence{kind:SourceKind::User,uri:format!("codewhale:memory:{id}"),locator:"Explicit correction".into(),sha256:None,observed_at:0}];
                draft.parent_ids.clear();
                let captured=b.store.capture(&b.access,&request_key,draft).map_err(fail)?;
                if let Some(replacement)=b.store.replacement_of(&b.access,&id).map_err(fail)? {
                    if replacement.id!=captured.memory.id {return Err(fail(codewhale_memory::Error::RevisionConflict));}
                } else {
                    b.store.supersede(&b.access,(&id,revision),(&captured.memory.id,captured.memory.revision),validation.as_ref(),&b.snapshot).map_err(fail)?;
                }
            },
        }
        Ok(Json(json!({"ok":true,"refresh_required":true,"already_sent_context_cannot_be_retracted":true})))
    }).await.map_err(|_|internal())?
}
