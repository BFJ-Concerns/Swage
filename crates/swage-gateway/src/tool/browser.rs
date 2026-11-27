//! Semantic browser tools backed by the gateway-owned browser host.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_storage::NewAuditEvent;
use swage_tool::context::ToolContext;
use tokio::io::AsyncWriteExt;

use crate::browser::{BrowserActionAudit, BrowserActionKind, BrowserHost, BrowserObservation};
use crate::tool::Tool;
use crate::tool::workspace::{create_tool_output_artifact, tool_output_artifact_path};
use crate::tool_executor::GatewayToolContext;

const TEXT_SECRET_KEYS: &[&str] = &["text"];

pub fn browser_tools(host: Arc<BrowserHost>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(BrowserNavigateTool::new(Arc::clone(&host))),
        Arc::new(BrowserSnapshotTool::new(Arc::clone(&host))),
        Arc::new(BrowserClickTool::new(Arc::clone(&host))),
        Arc::new(BrowserTypeTool::new(Arc::clone(&host))),
        Arc::new(BrowserSelectTool::new(Arc::clone(&host))),
        Arc::new(BrowserScrollTool::new(host)),
    ]
}

struct BrowserNavigateTool {
    host: Arc<BrowserHost>,
}

impl BrowserNavigateTool {
    fn new(host: Arc<BrowserHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl Tool for BrowserNavigateTool {
    fn name(&self) -> &str {
        "browser_navigate"
    }

    fn description(&self) -> &str {
        "Open a web page in the platform's browser and return a snapshot of it. A snapshot \
         is a text outline of what the page shows: its elements with their roles, names and \
         states, each one that can be acted on carrying a ref, which the other `browser_*` \
         tools take. The result also gives the page's `url` and `title`. An outline too long \
         to return whole is cut, and `full_snapshot` then names a file holding all of it, \
         which `file_read` opens. The browser keeps its logins from one session to the next, \
         so a site the operator has signed in to opens signed in. Only what an outline can \
         say is seen: what an image shows, and anything drawn on a canvas, is not. For a \
         page that needs no login, script or interaction, `url_fetch` is lighter."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"url": {"type": "string", "description": "The address to open, complete with its `http://` or `https://` scheme."}},
            "required": ["url"]
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let url = string_param(&params, "url", self.name())?;
        let observation = self.host.navigate(ctx.session_id(), url).await?;
        finish_read_call(self.name(), ctx, observation, BrowserActionKind::Read).await
    }
}

struct BrowserSnapshotTool {
    host: Arc<BrowserHost>,
}

impl BrowserSnapshotTool {
    fn new(host: Arc<BrowserHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl Tool for BrowserSnapshotTool {
    fn name(&self) -> &str {
        "browser_snapshot"
    }

    fn description(&self) -> &str {
        "Return a new snapshot of the page this session's browser is on, without acting on \
         it: the text outline `browser_navigate` describes, with new refs. It shows what a \
         page has become when it changed by itself, as when content arrives late. It fails \
         until `browser_navigate` has opened a page in this session."
    }

    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, _params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let observation = self.host.snapshot(ctx.session_id()).await?;
        finish_read_call(self.name(), ctx, observation, BrowserActionKind::Read).await
    }
}

struct BrowserClickTool {
    host: Arc<BrowserHost>,
}

impl BrowserClickTool {
    fn new(host: Arc<BrowserHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl Tool for BrowserClickTool {
    fn name(&self) -> &str {
        "browser_click"
    }

    fn description(&self) -> &str {
        "Click an element of the current page, named by its ref, and return a snapshot of \
         the page as the click left it. A click acts on the site as the signed-in operator \
         would: it can submit, send, buy or delete, and nothing here undoes it."
    }

    fn parameters(&self) -> Value {
        ref_schema()
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let reference = string_param(&params, "ref", self.name())?;
        let (observation, audit) = self.host.click(ctx.session_id(), reference).await?;
        finish_action_call(self.name(), ctx, &self.host, observation, audit).await
    }
}

struct BrowserTypeTool {
    host: Arc<BrowserHost>,
}

impl BrowserTypeTool {
    fn new(host: Arc<BrowserHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl Tool for BrowserTypeTool {
    fn name(&self) -> &str {
        "browser_type"
    }

    fn description(&self) -> &str {
        "Enter text in a field of the current page, named by its ref, and return a snapshot \
         of the page afterwards. The text takes the place of whatever the field holds, or, \
         with `replace` false, is added to the end of it. With `submit` the form the field \
         belongs to is then submitted. `text` may be a registered secret's \
         reference in place of a value: the secret's value is then what is entered, and it \
         is kept out of the conversation, the record and the snapshots that follow."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "ref": {"type": "string", "description": "The element's ref, taken from the most recent snapshot. Every `browser_*` call returns a new snapshot with new refs, and a ref from an earlier one is refused, since the element it named may no longer be there."},
                "text": {"type": "string", "description": "The text to enter, or a registered secret's reference to enter that secret's value."},
                "submit": {"type": "boolean", "default": false, "description": "Once the text is entered, submit the form the field belongs to, as pressing Enter in the field would."},
                "replace": {"type": "boolean", "default": true, "description": "`true`, the default, makes the text the field's whole value. `false` keeps what the field already holds and adds the text to the end of it."}
            },
            "required": ["ref", "text"]
        })
    }

    fn device_secret_keys(&self) -> &[&str] {
        TEXT_SECRET_KEYS
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let reference = string_param(&params, "ref", self.name())?;
        let text = string_param(&params, "text", self.name())?;
        let submit = params
            .get("submit")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let replace = params
            .get("replace")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let (observation, audit) = self
            .host
            .type_text(ctx.session_id(), reference, text, replace, submit)
            .await?;
        finish_action_call(self.name(), ctx, &self.host, observation, audit).await
    }
}

struct BrowserSelectTool {
    host: Arc<BrowserHost>,
}

impl BrowserSelectTool {
    fn new(host: Arc<BrowserHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl Tool for BrowserSelectTool {
    fn name(&self) -> &str {
        "browser_select"
    }

    fn description(&self) -> &str {
        "Choose an option in a dropdown or list box of the current page, named by its ref, \
         and return a snapshot of the page afterwards. The option is matched exactly, by its \
         label or by its value, and one that matches neither fails the call saying so."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "ref": {"type": "string", "description": "The element's ref, taken from the most recent snapshot. Every `browser_*` call returns a new snapshot with new refs, and a ref from an earlier one is refused, since the element it named may no longer be there."},
                "value": {"type": "string", "description": "The option to choose: its label as the snapshot shows it, or its underlying value."}
            },
            "required": ["ref", "value"]
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let reference = string_param(&params, "ref", self.name())?;
        let value = string_param(&params, "value", self.name())?;
        let (observation, audit) = self.host.select(ctx.session_id(), reference, value).await?;
        finish_action_call(self.name(), ctx, &self.host, observation, audit).await
    }
}

struct BrowserScrollTool {
    host: Arc<BrowserHost>,
}

impl BrowserScrollTool {
    fn new(host: Arc<BrowserHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl Tool for BrowserScrollTool {
    fn name(&self) -> &str {
        "browser_scroll"
    }

    fn description(&self) -> &str {
        "Scroll the current page and return a snapshot of it afterwards: most of a screen \
         `up` or `down`, or to the element a ref names. Give `direction` or `ref`; given \
         both, the ref decides. Scrolling matters on a page that loads more of its content \
         as it is scrolled."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "direction": {"type": "string", "enum": ["up", "down"], "description": "Which way to scroll, by most of a screen."},
                "ref": {"type": "string", "description": "An element to bring into view, in place of a direction. The element's ref, taken from the most recent snapshot. Every `browser_*` call returns a new snapshot with new refs, and a ref from an earlier one is refused, since the element it named may no longer be there."}
            }
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        if params.get("ref").is_some_and(|value| {
            !value.is_null() && value.as_str().is_none_or(|text| text.trim().is_empty())
        }) {
            return Err(ToolError::InvalidParameters {
                name: self.name().into(),
                message: "ref must be a non-empty string".into(),
            });
        }
        let direction = params.get("direction").and_then(Value::as_str);
        let reference = params.get("ref").and_then(Value::as_str);
        if direction.is_none() && reference.is_none() {
            return Err(ToolError::InvalidParameters {
                name: self.name().to_string(),
                message: "provide either direction or ref".to_string(),
            });
        }
        let (observation, audit) = self
            .host
            .scroll(ctx.session_id(), direction, reference)
            .await?;
        finish_action_call(self.name(), ctx, &self.host, observation, audit).await
    }
}

async fn finish_read_call(
    tool_name: &str,
    ctx: &dyn ToolContext,
    observation: BrowserObservation,
    kind: BrowserActionKind,
) -> Result<Value, ToolError> {
    let full_snapshot = write_full_snapshot(ctx, &observation).await?;
    write_audit_event(
        tool_name,
        ctx,
        BrowserActionAudit {
            url_before: None,
            url_after: observation.url.clone(),
            title_after: observation.title.clone(),
            target: None,
            navigated: false,
        },
        full_snapshot.clone(),
        None,
    )
    .await?;
    Ok(observation_json(observation, full_snapshot, None, kind))
}

async fn finish_action_call(
    tool_name: &str,
    ctx: &dyn ToolContext,
    host: &BrowserHost,
    observation: BrowserObservation,
    audit: BrowserActionAudit,
) -> Result<Value, ToolError> {
    let full_snapshot = write_full_snapshot(ctx, &observation).await?;
    let screenshot = write_screenshot(tool_name, ctx, host, &audit).await?;
    write_audit_event(
        tool_name,
        ctx,
        audit,
        full_snapshot.clone(),
        screenshot.clone(),
    )
    .await?;
    Ok(observation_json(
        observation,
        full_snapshot,
        screenshot,
        BrowserActionKind::Action,
    ))
}

async fn write_full_snapshot(
    ctx: &dyn ToolContext,
    observation: &BrowserObservation,
) -> Result<Option<String>, ToolError> {
    let Some(full) = observation.full_snapshot.as_ref() else {
        return Ok(None);
    };
    let path = tool_output_artifact_path(ctx, "snapshot.txt");
    let mut file = create_tool_output_artifact(ctx, &path.absolute)
        .await
        .map_err(|error| artifact_error("snapshot", error))?;
    file.write_all(full.as_bytes())
        .await
        .map_err(|error| artifact_error("snapshot", error))?;
    file.flush()
        .await
        .map_err(|error| artifact_error("snapshot", error))?;
    Ok(Some(path.reported))
}

async fn write_screenshot(
    tool_name: &str,
    ctx: &dyn ToolContext,
    host: &BrowserHost,
    audit: &BrowserActionAudit,
) -> Result<Option<String>, ToolError> {
    let Some(bytes) = host
        .screenshot_after_action(ctx.session_id(), tool_name)
        .await?
    else {
        return Ok(None);
    };
    let path = tool_output_artifact_path(ctx, "screenshot.jpg");
    let mut file = create_tool_output_artifact(ctx, &path.absolute)
        .await
        .map_err(|error| artifact_error("screenshot", error))?;
    file.write_all(&bytes)
        .await
        .map_err(|error| artifact_error("screenshot", error))?;
    file.flush()
        .await
        .map_err(|error| artifact_error("screenshot", error))?;
    let _ = audit;
    Ok(Some(path.reported))
}

async fn write_audit_event(
    tool_name: &str,
    ctx: &dyn ToolContext,
    audit: BrowserActionAudit,
    full_snapshot: Option<String>,
    screenshot: Option<String>,
) -> Result<(), ToolError> {
    let Some(gateway_ctx) = ctx.as_any().downcast_ref::<GatewayToolContext>() else {
        return Ok(());
    };
    let summary = match &audit.target {
        Some(target) => format!(
            "{} \"{}\" — {}",
            tool_name.trim_start_matches("browser_"),
            target.name,
            host_for_summary(&audit.url_after)
        ),
        None => format!("opened {}", audit.url_after),
    };
    let session_id = uuid::Uuid::parse_str(ctx.session_id()).ok();
    let metadata = json!({
        "call_id": ctx.call_id(),
        "turn_id": ctx.turn_id(),
        "url_before": audit.url_before,
        "url_after": audit.url_after,
        "title_after": audit.title_after,
        "ref": audit.target.as_ref().map(|target| target.reference.clone()),
        "target_role": audit.target.as_ref().map(|target| target.role.clone()),
        "target_name": audit.target.as_ref().map(|target| target.name.clone()),
        "navigated": audit.navigated,
        "artifacts": {
            "screenshot": screenshot,
            "full_snapshot": full_snapshot,
        }
    });
    gateway_ctx
        .storage()
        .create_audit_event(NewAuditEvent {
            id: uuid::Uuid::new_v4().to_string(),
            session_id,
            entity_kind: "browser".to_string(),
            entity_id: "default".to_string(),
            event_kind: tool_name.to_string(),
            summary,
            metadata,
        })
        .await
        .map_err(|error| ToolError::ExecutionFailed {
            name: tool_name.to_string(),
            message: format!("failed to record browser audit event: {error}"),
        })?;
    Ok(())
}

fn observation_json(
    observation: BrowserObservation,
    full_snapshot: Option<String>,
    screenshot: Option<String>,
    _kind: BrowserActionKind,
) -> Value {
    json!({
        "output": observation.output,
        "url": observation.url,
        "title": observation.title,
        "snapshot": observation.snapshot_seq,
        "full_snapshot": full_snapshot,
        "screenshot": screenshot,
    })
}

fn ref_schema() -> Value {
    json!({
        "type": "object",
        "properties": {"ref": {"type": "string", "description": "The element's ref, taken from the most recent snapshot. Every `browser_*` call returns a new snapshot with new refs, and a ref from an earlier one is refused, since the element it named may no longer be there."}},
        "required": ["ref"]
    })
}

fn string_param<'a>(params: &'a Value, key: &str, tool_name: &str) -> Result<&'a str, ToolError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::InvalidParameters {
            name: tool_name.to_string(),
            message: format!("missing required argument: {key}"),
        })
}

fn artifact_error(kind: &str, error: std::io::Error) -> ToolError {
    ToolError::ExecutionFailed {
        name: "browser".to_string(),
        message: format!("failed to write browser {kind} artifact: {error}"),
    }
}

fn host_for_summary(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_else(|| url.to_string())
}
