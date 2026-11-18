use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::spec::{
    RichToolResult, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, optional_str,
    required_str, required_u64,
};
use crate::design::{DesignError, DesignStore, NewArtboard, WriteMode, WriteReport};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesignOp {
    List,
    CreateArtboard,
    WriteHtml,
    ReadHtml,
    Screenshot,
    Comments,
}

pub const ALL_DESIGN_OPS: [DesignOp; 6] = [
    DesignOp::List,
    DesignOp::CreateArtboard,
    DesignOp::WriteHtml,
    DesignOp::ReadHtml,
    DesignOp::Screenshot,
    DesignOp::Comments,
];

pub struct DesignTool(pub DesignOp);

impl DesignTool {
    pub fn all() -> Vec<Arc<dyn ToolSpec>> {
        ALL_DESIGN_OPS
            .into_iter()
            .map(|op| Arc::new(DesignTool(op)) as Arc<dyn ToolSpec>)
            .collect()
    }
}

fn store(context: &ToolContext) -> DesignStore {
    DesignStore::for_workspace(&context.workspace)
}

fn map_err(err: DesignError) -> ToolError {
    match err {
        DesignError::Invalid(msg) | DesignError::NotFound(msg) => ToolError::invalid_input(msg),
        DesignError::Io(msg) => ToolError::execution_failed(msg),
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, DesignError> + Send + 'static,
) -> Result<T, ToolError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| ToolError::execution_failed("design store task failed"))?
        .map_err(map_err)
}

fn doc_field() -> Value {
    json!({ "type": "string", "description": "Design id: 1-64 lowercase letters, digits or dashes. Created on first artboard." })
}

fn artboard_field() -> Value {
    json!({ "type": "string", "description": "Artboard id as returned by design_create_artboard or design_list." })
}

fn report_text(verb: &str, doc: &str, report: &WriteReport) -> String {
    let mut text = match &report.artboard {
        Some(board) => format!(
            "{verb} artboard `{}` in design `{doc}` ({}x{} at {},{}; rev {}; {} bytes).",
            board.id, board.width, board.height, board.x, board.y, board.rev, report.bytes
        ),
        None => format!("{verb} design `{doc}`."),
    };
    if !report.assets_copied.is_empty() {
        text.push_str(&format!(
            " Copied {} local asset(s) into the design.",
            report.assets_copied.len()
        ));
    }
    if !report.refused_references.is_empty() {
        text.push_str(&format!(
            " Refused {} local reference(s): {}.",
            report.refused_references.len(),
            report.refused_references.join("; ")
        ));
    }
    if report.remote_references > 0 {
        text.push_str(&format!(
            " {} remote URL(s) will not load: artboards render with the network blocked; inline images as data: URLs or reference workspace files.",
            report.remote_references
        ));
    }
    text.push_str(" Call design_screenshot to check it.");
    text
}

async fn read_html_source(
    input: &Value,
    context: &ToolContext,
) -> Result<(String, Option<PathBuf>), ToolError> {
    let html = optional_str(input, "html")?;
    let html_file = optional_str(input, "html_file")?;
    match (html, html_file) {
        (Some(_), Some(_)) => Err(ToolError::invalid_input(
            "pass `html` or `html_file`, not both",
        )),
        (Some(html), None) => Ok((html.to_string(), Some(context.workspace.clone()))),
        (None, Some(file)) => {
            let path = context.resolve_path(file)?;
            let text = tokio::fs::read_to_string(&path).await.map_err(|err| {
                ToolError::execution_failed(format!("could not read {}: {err}", path.display()))
            })?;
            Ok((text, Some(context.workspace.clone())))
        }
        (None, None) => Ok((String::new(), None)),
    }
}

#[async_trait]
impl ToolSpec for DesignTool {
    fn name(&self) -> &'static str {
        match self.0 {
            DesignOp::List => "design_list",
            DesignOp::CreateArtboard => "design_create_artboard",
            DesignOp::WriteHtml => "design_write_html",
            DesignOp::ReadHtml => "design_read_html",
            DesignOp::Screenshot => "design_screenshot",
            DesignOp::Comments => "design_comments",
        }
    }

    fn description(&self) -> &'static str {
        match self.0 {
            DesignOp::List => {
                "List Makewhale designs in this workspace, or the artboards and open comments of one design. Designs are HTML/CSS artboards on the desktop design canvas."
            }
            DesignOp::CreateArtboard => {
                "Create an HTML/CSS artboard on a Makewhale design canvas (the design is created if new). Provide body HTML (no <html>/<head>) as `html` or a workspace file as `html_file`. The codewhale-design tokens (CSS variables such as --surface, --text, --accent) and Geist / Geist Mono fonts are always available. Scripts never run and the network is blocked; workspace image files referenced by file:// URLs are copied into the design. Returns the artboard id."
            }
            DesignOp::WriteHtml => {
                "Write HTML into an existing Makewhale artboard. mode `replace` swaps the whole body; `append` inserts the HTML inside the artboard's root element, so build a design in several small writes. The desktop canvas live-reloads after each write."
            }
            DesignOp::ReadHtml => {
                "Read back an artboard's current body HTML so you can edit it precisely."
            }
            DesignOp::Screenshot => {
                "Render an artboard in a sandboxed headless browser (no scripts, no network) and return the PNG so you can review it visually. The image is saved in the design's shots folder."
            }
            DesignOp::Comments => {
                "List the comments people pinned to a design's artboards, optionally resolving one by id after you address it."
            }
        }
    }

    fn input_schema(&self) -> Value {
        match self.0 {
            DesignOp::List => json!({
                "type": "object",
                "properties": { "doc": doc_field() }
            }),
            DesignOp::CreateArtboard => json!({
                "type": "object",
                "properties": {
                    "doc": doc_field(),
                    "name": { "type": "string", "description": "Human name for the artboard, e.g. 'Settings · Dark'." },
                    "width": { "type": "integer", "minimum": 1, "maximum": 8192 },
                    "height": { "type": "integer", "minimum": 1, "maximum": 8192 },
                    "theme": { "type": "string", "enum": ["dark", "light"], "description": "Sets data-theme on the document root. Default dark." },
                    "x": { "type": "integer", "description": "Canvas position; default places it right of the last artboard." },
                    "y": { "type": "integer" },
                    "html": { "type": "string", "description": "Initial body HTML." },
                    "html_file": { "type": "string", "description": "Workspace path of an HTML fragment to load instead of `html`." }
                },
                "required": ["doc", "name", "width", "height"]
            }),
            DesignOp::WriteHtml => json!({
                "type": "object",
                "properties": {
                    "doc": doc_field(),
                    "artboard": artboard_field(),
                    "html": { "type": "string" },
                    "html_file": { "type": "string" },
                    "mode": { "type": "string", "enum": ["replace", "append"], "description": "Default append." }
                },
                "required": ["doc", "artboard"]
            }),
            DesignOp::ReadHtml => json!({
                "type": "object",
                "properties": { "doc": doc_field(), "artboard": artboard_field() },
                "required": ["doc", "artboard"]
            }),
            DesignOp::Screenshot => json!({
                "type": "object",
                "properties": {
                    "doc": doc_field(),
                    "artboard": artboard_field(),
                    "scale": { "type": "number", "minimum": 0.25, "maximum": 3, "description": "Device scale factor. Default 1." }
                },
                "required": ["doc", "artboard"]
            }),
            DesignOp::Comments => json!({
                "type": "object",
                "properties": {
                    "doc": doc_field(),
                    "artboard": artboard_field(),
                    "resolve": { "type": "string", "description": "Comment id to mark resolved." }
                },
                "required": ["doc"]
            }),
        }
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        match self.0 {
            DesignOp::CreateArtboard | DesignOp::WriteHtml | DesignOp::Comments => {
                vec![ToolCapability::WritesFiles]
            }
            DesignOp::List | DesignOp::ReadHtml | DesignOp::Screenshot => {
                vec![ToolCapability::ReadOnly]
            }
        }
    }

    fn defer_loading(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        self.execute_rich(input, context)
            .await
            .map(RichToolResult::into_result)
    }

    async fn execute_rich(
        &self,
        input: Value,
        context: &ToolContext,
    ) -> Result<RichToolResult, ToolError> {
        let store = store(context);
        match self.0 {
            DesignOp::List => {
                let doc = optional_str(&input, "doc")?.map(str::to_string);
                let result = match doc {
                    Some(doc) => {
                        let loaded = blocking(move || store.load(&doc)).await?;
                        let open: Vec<_> = loaded.comments.iter().filter(|c| !c.resolved).collect();
                        let lines: Vec<String> = loaded
                            .artboards
                            .iter()
                            .map(|a| {
                                format!(
                                    "- {} \"{}\" {}x{} at {},{} {} rev {}",
                                    a.id, a.name, a.width, a.height, a.x, a.y, a.theme, a.rev
                                )
                            })
                            .collect();
                        ToolResult::success(format!(
                            "Design `{}` ({} artboards, {} open comments, rev {}):\n{}",
                            loaded.id,
                            loaded.artboards.len(),
                            open.len(),
                            loaded.rev,
                            lines.join("\n")
                        ))
                        .with_metadata(json!({ "design": loaded }))
                    }
                    None => {
                        let all = blocking(move || store.list()).await?;
                        let lines: Vec<String> = all
                            .iter()
                            .map(|d| {
                                format!(
                                    "- {} \"{}\": {} artboards, {} open comments",
                                    d.id, d.name, d.artboards, d.open_comments
                                )
                            })
                            .collect();
                        let text = if lines.is_empty() {
                            "No designs yet. Create one with design_create_artboard.".to_string()
                        } else {
                            format!("Designs:\n{}", lines.join("\n"))
                        };
                        ToolResult::success(text).with_metadata(json!({ "designs": all }))
                    }
                };
                Ok(RichToolResult::plain(result))
            }
            DesignOp::CreateArtboard => {
                let doc = required_str(&input, "doc")?.to_string();
                let width = u32::try_from(required_u64(&input, "width")?)
                    .map_err(|_| ToolError::invalid_input("width is too large"))?;
                let height = u32::try_from(required_u64(&input, "height")?)
                    .map_err(|_| ToolError::invalid_input("height is too large"))?;
                let (html, source_dir) = read_html_source(&input, context).await?;
                let spec = NewArtboard {
                    name: required_str(&input, "name")?.to_string(),
                    width,
                    height,
                    theme: optional_str(&input, "theme")?.map(str::to_string),
                    x: input.get("x").and_then(Value::as_i64),
                    y: input.get("y").and_then(Value::as_i64),
                    html,
                    source_dir,
                };
                let doc_for = doc.clone();
                let report = blocking(move || store.create_artboard(&doc_for, spec)).await?;
                Ok(RichToolResult::plain(
                    ToolResult::success(report_text("Created", &doc, &report))
                        .with_metadata(json!({ "design_write": report, "doc": doc })),
                ))
            }
            DesignOp::WriteHtml => {
                let doc = required_str(&input, "doc")?.to_string();
                let artboard = required_str(&input, "artboard")?.to_string();
                let mode = match optional_str(&input, "mode")?.unwrap_or("append") {
                    "replace" => WriteMode::Replace,
                    "append" => WriteMode::Append,
                    other => {
                        return Err(ToolError::invalid_input(format!(
                            "unknown mode `{other}`; use replace or append"
                        )));
                    }
                };
                let (html, source_dir) = read_html_source(&input, context).await?;
                if html.is_empty() && mode == WriteMode::Append {
                    return Err(ToolError::invalid_input(
                        "append needs `html` or `html_file`",
                    ));
                }
                let doc_for = doc.clone();
                let report = blocking(move || {
                    store.write_html(&doc_for, &artboard, &html, mode, source_dir.as_deref())
                })
                .await?;
                Ok(RichToolResult::plain(
                    ToolResult::success(report_text("Updated", &doc, &report))
                        .with_metadata(json!({ "design_write": report, "doc": doc })),
                ))
            }
            DesignOp::ReadHtml => {
                let doc = required_str(&input, "doc")?.to_string();
                let artboard = required_str(&input, "artboard")?.to_string();
                let html = blocking(move || store.read_fragment(&doc, &artboard)).await?;
                Ok(RichToolResult::plain(ToolResult::success(html)))
            }
            DesignOp::Screenshot => {
                let doc = required_str(&input, "doc")?.to_string();
                let artboard = required_str(&input, "artboard")?.to_string();
                let scale = input.get("scale").and_then(Value::as_f64).unwrap_or(1.0) as f32;
                let shot = store
                    .screenshot(&doc, &artboard, scale)
                    .await
                    .map_err(map_err)?;
                let mut rich = super::read_media::execute_read_media(
                    json!({ "path": shot.display().to_string(), "detail": "auto" }),
                    context,
                )
                .await?;
                let note = format!(
                    "Rendered artboard `{artboard}` of design `{doc}` to {}.\n",
                    shot.display()
                );
                rich.result.content = format!("{note}{}", rich.result.content);
                Ok(rich)
            }
            DesignOp::Comments => {
                let doc = required_str(&input, "doc")?.to_string();
                let artboard = optional_str(&input, "artboard")?.map(str::to_string);
                let resolve = optional_str(&input, "resolve")?.map(str::to_string);
                let loaded = blocking(move || {
                    if let Some(id) = resolve {
                        store.set_comment_resolved(&doc, &id, true)?;
                    }
                    store.load(&doc)
                })
                .await?;
                let comments: Vec<_> = loaded
                    .comments
                    .iter()
                    .filter(|c| artboard.as_deref().is_none_or(|a| c.artboard_id == a))
                    .collect();
                let lines: Vec<String> = comments
                    .iter()
                    .map(|c| {
                        format!(
                            "- [{}] {} on {} at ({:.0}%, {:.0}%) by {}: {}",
                            if c.resolved { "resolved" } else { "open" },
                            c.id,
                            c.artboard_id,
                            c.x * 100.0,
                            c.y * 100.0,
                            c.author,
                            c.body
                        )
                    })
                    .collect();
                let text = if lines.is_empty() {
                    format!("No comments on design `{}`.", loaded.id)
                } else {
                    lines.join("\n")
                };
                Ok(RichToolResult::plain(
                    ToolResult::success(text).with_metadata(json!({ "comments": comments })),
                ))
            }
        }
    }
}
