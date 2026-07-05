//! Gateway-owned browser host for the semantic browser tools.
//!
//! The browser is deliberately hosted here, not in agent sandboxes. Agents see
//! only stable refs from the accessibility snapshot; CDP ids stay inside this
//! module as implementation detail.

mod render;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chromiumoxide::cdp::browser_protocol::accessibility::{
    AxNode, EnableParams, GetFullAxTreeParams, QueryAxTreeParams,
};
use chromiumoxide::cdp::browser_protocol::dom::{
    BackendNodeId, GetDocumentParams, ResolveNodeParams,
};
use chromiumoxide::cdp::browser_protocol::page::{
    CaptureScreenshotFormat, CaptureScreenshotParams, NavigateParams,
};
use chromiumoxide::cdp::js_protocol::runtime::{CallArgument, CallFunctionOnParams};
use chromiumoxide::{Browser, Page};
use futures::StreamExt;
use reqwest::Url;
use serde::Deserialize;
use serde_json::json;
use swage_config::{BrowserScreenshotPolicy, BrowserToolConfig};
use swage_core::ToolError;
use swage_sandbox::ChildProcessManager;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

pub use render::{RefEntry, RefTable, RenderedSnapshot, render_snapshot};

const DEFAULT_PROFILE: &str = "default";
const LOAD_SETTLE: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub struct BrowserConfig {
    pub binary_path: Option<String>,
    pub profile_root: PathBuf,
    pub page_idle: Duration,
    pub process_idle: Duration,
    pub max_snapshot_bytes: usize,
    pub screenshots: BrowserScreenshotPolicy,
    pub allow_hosts: Vec<String>,
}

impl BrowserConfig {
    pub fn from_tool_config(config: &BrowserToolConfig) -> Result<Self, ToolError> {
        let root = swage_config::resolve_root().map_err(|problem| ToolError::ExecutionFailed {
            name: "browser".to_string(),
            message: format!("failed to resolve Swage root for browser profile: {problem}"),
        })?;
        Ok(Self {
            binary_path: config.binary_path.clone(),
            profile_root: root.join("browser").join("profiles"),
            page_idle: Duration::from_secs(config.page_idle_secs),
            process_idle: Duration::from_secs(config.process_idle_secs),
            max_snapshot_bytes: config.max_snapshot_bytes,
            screenshots: config.screenshots.clone(),
            allow_hosts: config.allow_hosts.clone(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserActionKind {
    Read,
    Action,
}

#[derive(Debug, Clone)]
pub struct BrowserObservation {
    pub output: String,
    pub url: String,
    pub title: String,
    pub snapshot_seq: u64,
    pub full_snapshot: Option<String>,
    pub refs: RefTable,
}

#[derive(Debug, Clone)]
pub struct BrowserActionAudit {
    pub url_before: Option<String>,
    pub url_after: String,
    pub title_after: String,
    pub target: Option<RefEntry>,
    pub navigated: bool,
}

pub struct BrowserHost {
    config: BrowserConfig,
    inner: Mutex<BrowserState>,
}

struct BrowserState {
    process: Option<BrowserProcess>,
    sessions: HashMap<String, PageSession>,
}

struct BrowserProcess {
    child: ChildProcessManager,
    browser: Browser,
    handler: JoinHandle<()>,
    last_used: Instant,
}

struct PageSession {
    page: Page,
    refs: RefTable,
    masked_backend_nodes: HashSet<BackendNodeId>,
    masked_selectors: HashSet<String>,
    snapshot_seq: u64,
    last_used: Instant,
}

impl BrowserHost {
    pub fn new(config: BrowserConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            inner: Mutex::new(BrowserState {
                process: None,
                sessions: HashMap::new(),
            }),
        })
    }

    pub async fn shutdown(&self) {
        let mut state = self.inner.lock().await;
        state.sessions.clear();
        if let Some(mut process) = state.process.take() {
            if let Err(error) = process.browser.close().await {
                debug!(%error, "browser CDP close failed during shutdown");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            process.handler.abort();
            if let Err(error) = process
                .child
                .graceful_shutdown(Duration::from_secs(2))
                .await
            {
                warn!(%error, "browser process did not shut down cleanly");
            }
        }
    }

    pub async fn sweep_idle(&self) {
        let mut state = self.inner.lock().await;
        let now = Instant::now();
        state
            .sessions
            .retain(|_, session| now.duration_since(session.last_used) < self.config.page_idle);
        let should_stop_process = state.sessions.is_empty()
            && state.process.as_ref().is_some_and(|process| {
                now.duration_since(process.last_used) >= self.config.process_idle
            });
        if should_stop_process && let Some(mut process) = state.process.take() {
            if let Err(error) = process.browser.close().await {
                debug!(%error, "browser CDP close failed during idle sweep");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            process.handler.abort();
            if let Err(error) = process
                .child
                .graceful_shutdown(Duration::from_secs(2))
                .await
            {
                warn!(%error, "idle browser process did not shut down cleanly");
            }
        }
    }

    pub async fn navigate(
        &self,
        session_id: &str,
        url: &str,
    ) -> Result<BrowserObservation, ToolError> {
        let parsed = parse_browser_url(url)?;
        self.ensure_navigation_is_allowed(&parsed).await?;

        let mut state = self.inner.lock().await;
        state.ensure_process(&self.config).await?;
        let page = state.ensure_page(session_id).await?;
        page.page
            .goto(NavigateParams::new(parsed.as_str()))
            .await
            .map_err(browser_error("browser_navigate", "failed to navigate"))?;
        tokio::time::sleep(LOAD_SETTLE).await;
        page.refs.clear();
        page.masked_backend_nodes.clear();
        page.masked_selectors.clear();
        self.observe_locked(page, BrowserActionKind::Read).await
    }

    pub async fn snapshot(&self, session_id: &str) -> Result<BrowserObservation, ToolError> {
        let mut state = self.inner.lock().await;
        let Some(page) = state.sessions.get_mut(session_id) else {
            return Err(ToolError::ExecutionFailed {
                name: "browser_snapshot".to_string(),
                message: "navigate first with browser_navigate before taking a snapshot"
                    .to_string(),
            });
        };
        self.observe_locked(page, BrowserActionKind::Read).await
    }

    pub async fn click(
        &self,
        session_id: &str,
        reference: &str,
    ) -> Result<(BrowserObservation, BrowserActionAudit), ToolError> {
        self.act_on_ref(session_id, reference, "browser_click", false, |object_id| {
            CallFunctionOnParams::builder()
                .function_declaration(
                    "function () { this.scrollIntoView({block: 'center', inline: 'center'}); this.click(); }",
                )
                .object_id(object_id)
                .user_gesture(true)
                .await_promise(true)
                .build()
                .expect("valid click function")
        })
        .await
    }

    pub async fn type_text(
        &self,
        session_id: &str,
        reference: &str,
        text: &str,
        replace: bool,
        submit: bool,
    ) -> Result<(BrowserObservation, BrowserActionAudit), ToolError> {
        let quoted_text = serde_json::to_string(text).expect("text serialises");
        if let Some(result) = self
            .act_on_selector_ref(
                session_id,
                reference,
                "browser_type",
                true,
                format!(
                    "(el) => {{ el.scrollIntoView({{block:'center',inline:'center'}}); el.focus(); if ({replace}) el.value = ''; el.value = {quoted_text}; el.dispatchEvent(new InputEvent('input', {{bubbles:true}})); el.dispatchEvent(new Event('change', {{bubbles:true}})); if ({submit} && el.form) el.form.requestSubmit(); }}"
                ),
            )
            .await?
        {
            return Ok(result);
        }
        let text_value = json!(text);
        self.act_on_ref(session_id, reference, "browser_type", true, move |object_id| {
            CallFunctionOnParams::builder()
                .function_declaration(
                    "function (text, replace, submit) {
                        this.scrollIntoView({block: 'center', inline: 'center'});
                        this.focus();
                        if (replace) {
                            this.value = '';
                            this.dispatchEvent(new InputEvent('input', {bubbles: true, inputType: 'deleteContent'}));
                        }
                        this.value = String(text);
                        this.dispatchEvent(new InputEvent('input', {bubbles: true, data: String(text)}));
                        this.dispatchEvent(new Event('change', {bubbles: true}));
                        if (submit) {
                            this.dispatchEvent(new KeyboardEvent('keydown', {key: 'Enter', bubbles: true}));
                            const form = this.form;
                            if (form) form.requestSubmit();
                        }
                    }",
                )
                .object_id(object_id)
                .argument(CallArgument::builder().value(text_value.clone()).build())
                .argument(CallArgument::builder().value(json!(replace)).build())
                .argument(CallArgument::builder().value(json!(submit)).build())
                .user_gesture(true)
                .await_promise(true)
                .build()
                .expect("valid type function")
        })
        .await
    }

    pub async fn select(
        &self,
        session_id: &str,
        reference: &str,
        value: &str,
    ) -> Result<(BrowserObservation, BrowserActionAudit), ToolError> {
        let quoted_value = serde_json::to_string(value).expect("value serialises");
        if let Some(result) = self
            .act_on_selector_ref(
                session_id,
                reference,
                "browser_select",
                false,
                format!(
                    "(el) => {{ const wanted = {quoted_value}; const option = Array.from(el.options || []).find(o => o.value === wanted || o.label === wanted || o.textContent.trim() === wanted); if (!option) throw new Error('option not found: ' + wanted); el.value = option.value; el.dispatchEvent(new Event('input', {{bubbles:true}})); el.dispatchEvent(new Event('change', {{bubbles:true}})); }}"
                ),
            )
            .await?
        {
            return Ok(result);
        }
        let option_value = json!(value);
        self.act_on_ref(session_id, reference, "browser_select", false, move |object_id| {
            CallFunctionOnParams::builder()
                .function_declaration(
                    "function (wanted) {
                        this.scrollIntoView({block: 'center', inline: 'center'});
                        this.focus();
                        const wantedString = String(wanted);
                        const options = Array.from(this.options || []);
                        const option = options.find(o => o.value === wantedString || o.label === wantedString || o.textContent.trim() === wantedString);
                        if (!option) throw new Error('option not found: ' + wantedString);
                        this.value = option.value;
                        this.dispatchEvent(new Event('input', {bubbles: true}));
                        this.dispatchEvent(new Event('change', {bubbles: true}));
                    }",
                )
                .object_id(object_id)
                .argument(CallArgument::builder().value(option_value.clone()).build())
                .user_gesture(true)
                .await_promise(true)
                .build()
                .expect("valid select function")
        })
        .await
    }

    pub async fn scroll(
        &self,
        session_id: &str,
        direction: Option<&str>,
        reference: Option<&str>,
    ) -> Result<(BrowserObservation, BrowserActionAudit), ToolError> {
        let mut state = self.inner.lock().await;
        let Some(page) = state.sessions.get_mut(session_id) else {
            return Err(ToolError::ExecutionFailed {
                name: "browser_scroll".to_string(),
                message: "navigate first with browser_navigate before scrolling".to_string(),
            });
        };
        let url_before = current_url(&page.page).await.ok();
        let target = if let Some(reference) = reference {
            let entry = page
                .refs
                .get(reference)
                .cloned()
                .ok_or_else(|| stale_ref_error("browser_scroll"))?;
            let object_id =
                resolve_object(&page.page, entry.backend_node_id, "browser_scroll").await?;
            page.page
                .execute(
                    CallFunctionOnParams::builder()
                        .function_declaration(
                            "function () { this.scrollIntoView({block: 'center', inline: 'center'}); }",
                        )
                        .object_id(object_id)
                        .user_gesture(true)
                        .await_promise(true)
                        .build()
                        .expect("valid scroll-to-ref function"),
                )
                .await
                .map_err(browser_error("browser_scroll", "that element is no longer on the page — call browser_snapshot for the current state"))?;
            Some(entry)
        } else {
            let dir = direction.unwrap_or("down");
            let delta = match dir {
                "down" => 1,
                "up" => -1,
                _ => {
                    return Err(ToolError::InvalidParameters {
                        name: "browser_scroll".to_string(),
                        message: "direction must be \"up\" or \"down\"".to_string(),
                    });
                }
            };
            page.page
                .evaluate_function(format!(
                    "() => window.scrollBy({{top: {} * window.innerHeight * 0.9, behavior: 'instant'}})",
                    delta
                ))
                .await
                .map_err(browser_error("browser_scroll", "failed to scroll page"))?;
            None
        };
        tokio::time::sleep(LOAD_SETTLE).await;
        let observation = self.observe_locked(page, BrowserActionKind::Action).await?;
        let audit = BrowserActionAudit {
            url_before: url_before.clone(),
            url_after: observation.url.clone(),
            title_after: observation.title.clone(),
            target,
            navigated: url_before.as_deref() != Some(observation.url.as_str()),
        };
        Ok((observation, audit))
    }

    pub async fn screenshot_after_action(
        &self,
        session_id: &str,
    ) -> Result<Option<Vec<u8>>, ToolError> {
        if self.config.screenshots == BrowserScreenshotPolicy::Off {
            return Ok(None);
        }
        let mut state = self.inner.lock().await;
        let Some(page) = state.sessions.get_mut(session_id) else {
            return Ok(None);
        };
        let bytes = page
            .page
            .screenshot(
                CaptureScreenshotParams::builder()
                    .format(CaptureScreenshotFormat::Jpeg)
                    .quality(60)
                    .build(),
            )
            .await
            .map_err(browser_error(
                "browser",
                "failed to capture browser screenshot",
            ))?;
        Ok(Some(bytes))
    }

    async fn act_on_ref<F>(
        &self,
        session_id: &str,
        reference: &str,
        tool_name: &str,
        mask_target_after_action: bool,
        build_call: F,
    ) -> Result<(BrowserObservation, BrowserActionAudit), ToolError>
    where
        F: FnOnce(chromiumoxide::cdp::js_protocol::runtime::RemoteObjectId) -> CallFunctionOnParams,
    {
        let mut state = self.inner.lock().await;
        let Some(page) = state.sessions.get_mut(session_id) else {
            return Err(ToolError::ExecutionFailed {
                name: tool_name.to_string(),
                message: "navigate first with browser_navigate before acting on the page"
                    .to_string(),
            });
        };
        let target = page
            .refs
            .get(reference)
            .cloned()
            .ok_or_else(|| stale_ref_error(tool_name))?;
        let url_before = current_url(&page.page).await.ok();
        if let Some(selector) = target.selector.clone() {
            page.page
                .evaluate_function(format!(
                    "() => {{ const el = document.querySelector({}); if (!el) throw new Error('element no longer exists'); el.scrollIntoView({{block:'center',inline:'center'}}); if (el.form && (el.type === 'submit' || el.tagName.toLowerCase() === 'button')) el.form.submit(); else el.click(); }}",
                    serde_json::to_string(&selector).expect("selector serialises")
                ))
                .await
                .map_err(browser_error(
                    tool_name,
                    "that element is no longer on the page — call browser_snapshot for the current state",
                ))?;
            if mask_target_after_action {
                page.mask_ref_target(&target);
            }
            tokio::time::sleep(LOAD_SETTLE).await;
            let observation = self.observe_locked(page, BrowserActionKind::Action).await?;
            let audit = BrowserActionAudit {
                url_before: url_before.clone(),
                url_after: observation.url.clone(),
                title_after: observation.title.clone(),
                target: Some(target),
                navigated: url_before.as_deref() != Some(observation.url.as_str()),
            };
            return Ok((observation, audit));
        }
        let object_id = resolve_object(&page.page, target.backend_node_id, tool_name).await?;
        page.page
            .execute(build_call(object_id))
            .await
            .map_err(browser_error(
                tool_name,
                "that element is no longer on the page — call browser_snapshot for the current state",
            ))?;
        if mask_target_after_action {
            page.mask_ref_target(&target);
        }
        tokio::time::sleep(LOAD_SETTLE).await;
        let observation = self.observe_locked(page, BrowserActionKind::Action).await?;
        let audit = BrowserActionAudit {
            url_before: url_before.clone(),
            url_after: observation.url.clone(),
            title_after: observation.title.clone(),
            target: Some(target),
            navigated: url_before.as_deref() != Some(observation.url.as_str()),
        };
        Ok((observation, audit))
    }

    async fn act_on_selector_ref(
        &self,
        session_id: &str,
        reference: &str,
        tool_name: &str,
        mask_target_after_action: bool,
        function: String,
    ) -> Result<Option<(BrowserObservation, BrowserActionAudit)>, ToolError> {
        let mut state = self.inner.lock().await;
        let Some(page) = state.sessions.get_mut(session_id) else {
            return Ok(None);
        };
        let Some(target) = page.refs.get(reference).cloned() else {
            return Ok(None);
        };
        let Some(selector) = target.selector.clone() else {
            return Ok(None);
        };
        let url_before = current_url(&page.page).await.ok();
        page.page
            .evaluate_function(format!(
                "() => {{ const el = document.querySelector({}); if (!el) throw new Error('element no longer exists'); ({})(el); }}",
                serde_json::to_string(&selector).expect("selector serialises"),
                function
            ))
            .await
            .map_err(browser_error(
                tool_name,
                "that element is no longer on the page — call browser_snapshot for the current state",
            ))?;
        if mask_target_after_action {
            page.mask_ref_target(&target);
        }
        tokio::time::sleep(LOAD_SETTLE).await;
        let observation = self.observe_locked(page, BrowserActionKind::Action).await?;
        let audit = BrowserActionAudit {
            url_before: url_before.clone(),
            url_after: observation.url.clone(),
            title_after: observation.title.clone(),
            target: Some(target),
            navigated: url_before.as_deref() != Some(observation.url.as_str()),
        };
        Ok(Some((observation, audit)))
    }

    async fn observe_locked(
        &self,
        page: &mut PageSession,
        kind: BrowserActionKind,
    ) -> Result<BrowserObservation, ToolError> {
        page.last_used = Instant::now();
        page.snapshot_seq += 1;
        page.page
            .execute(EnableParams::default())
            .await
            .map_err(browser_error(
                "browser_snapshot",
                "failed to enable accessibility tree",
            ))?;
        let mut nodes = page
            .page
            .execute(GetFullAxTreeParams::builder().depth(100).build())
            .await
            .map_err(browser_error(
                "browser_snapshot",
                "failed to read accessibility tree",
            ))?
            .result
            .nodes;
        if nodes.len() <= 1 {
            nodes = query_accessibility_tree(&page.page).await?;
        }
        let url = current_url(&page.page).await?;
        let title = current_title(&page.page).await?;
        let rendered = render_snapshot(
            &nodes,
            &url,
            &title,
            page.snapshot_seq,
            self.config.max_snapshot_bytes,
            &page.masked_backend_nodes,
        );
        if rendered.refs.is_empty()
            || rendered
                .refs
                .values()
                .all(|entry| entry.role.eq_ignore_ascii_case("RootWebArea"))
        {
            let rendered = dom_snapshot(
                &page.page,
                &url,
                &title,
                page.snapshot_seq,
                self.config.max_snapshot_bytes,
                &page.masked_selectors,
            )
            .await?;
            page.refs = rendered.refs.clone();
            return Ok(BrowserObservation {
                output: rendered.output,
                url,
                title,
                snapshot_seq: page.snapshot_seq,
                full_snapshot: rendered.full_output,
                refs: rendered.refs,
            });
        }
        page.refs = rendered.refs.clone();
        let include_output = matches!(kind, BrowserActionKind::Action | BrowserActionKind::Read);
        Ok(BrowserObservation {
            output: if include_output {
                rendered.output
            } else {
                String::new()
            },
            url,
            title,
            snapshot_seq: page.snapshot_seq,
            full_snapshot: rendered.full_output,
            refs: rendered.refs,
        })
    }

    async fn ensure_navigation_is_allowed(&self, url: &Url) -> Result<(), ToolError> {
        if let Some(host) = url.host_str()
            && self
                .config
                .allow_hosts
                .iter()
                .any(|allowed| allowed == host)
        {
            return Ok(());
        }
        crate::tool::url_fetch::ensure_url_target_is_safe(url, "browser_navigate").await
    }
}

impl BrowserState {
    async fn ensure_process(&mut self, config: &BrowserConfig) -> Result<(), ToolError> {
        if let Some(process) = &mut self.process {
            process.last_used = Instant::now();
            if process.child.is_running() {
                return Ok(());
            }
            process.handler.abort();
            self.sessions.clear();
            self.process = None;
            return Err(ToolError::ExecutionFailed {
                name: "browser".to_string(),
                message: "the browser restarted and page state was lost — navigate again"
                    .to_string(),
            });
        }

        self.process = Some(match spawn_browser(config, false).await {
            Ok(process) => process,
            Err(sandboxed_error) => {
                warn!(error = %sandboxed_error, "sandboxed Chromium launch failed; retrying with --no-sandbox");
                spawn_browser(config, true).await?
            }
        });
        Ok(())
    }

    async fn ensure_page(&mut self, session_id: &str) -> Result<&mut PageSession, ToolError> {
        if !self.sessions.contains_key(session_id) {
            let Some(process) = &self.process else {
                return Err(ToolError::ExecutionFailed {
                    name: "browser".to_string(),
                    message: "browser process is not available".to_string(),
                });
            };
            let page = process
                .browser
                .new_page("about:blank")
                .await
                .map_err(browser_error("browser", "failed to create browser page"))?;
            self.sessions.insert(
                session_id.to_string(),
                PageSession {
                    page,
                    refs: HashMap::new(),
                    masked_backend_nodes: HashSet::new(),
                    masked_selectors: HashSet::new(),
                    snapshot_seq: 0,
                    last_used: Instant::now(),
                },
            );
        }
        Ok(self
            .sessions
            .get_mut(session_id)
            .expect("session just inserted"))
    }
}

impl PageSession {
    fn mask_ref_target(&mut self, target: &RefEntry) {
        if target.backend_node_id != BackendNodeId::new(0) {
            self.masked_backend_nodes.insert(target.backend_node_id);
        }
        if let Some(selector) = &target.selector {
            self.masked_selectors.insert(selector.clone());
        }
    }
}

async fn spawn_browser(
    config: &BrowserConfig,
    no_sandbox: bool,
) -> Result<BrowserProcess, ToolError> {
    let binary = resolve_browser_binary(config.binary_path.as_deref())?;
    let port = reserve_local_port()?;
    let profile = config.profile_root.join(DEFAULT_PROFILE);
    tokio::fs::create_dir_all(&profile)
        .await
        .map_err(|error| ToolError::ExecutionFailed {
            name: "browser".to_string(),
            message: format!(
                "failed to create browser profile '{}': {error}",
                profile.display()
            ),
        })?;

    let mut command = tokio::process::Command::new(binary);
    command
        .arg("--headless=new")
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("--disable-gpu")
        .arg("--force-renderer-accessibility")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--remote-debugging-address=127.0.0.1")
        .arg(format!("--remote-debugging-port={port}"))
        .arg("about:blank")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if no_sandbox {
        command.arg("--no-sandbox");
    }
    let child =
        ChildProcessManager::spawn(command).map_err(|error| ToolError::ExecutionFailed {
            name: "browser".to_string(),
            message: format!("failed to start Chromium: {error}"),
        })?;
    let endpoint = format!("http://127.0.0.1:{port}");
    wait_for_devtools(&endpoint).await?;
    let (browser, mut handler) = Browser::connect(endpoint).await.map_err(browser_error(
        "browser",
        "failed to connect to Chromium DevTools",
    ))?;
    let handler = tokio::spawn(async move {
        while let Some(event) = handler.next().await {
            if let Err(error) = event {
                debug!(%error, "browser CDP handler event failed");
            }
        }
    });
    Ok(BrowserProcess {
        child,
        browser,
        handler,
        last_used: Instant::now(),
    })
}

fn resolve_browser_binary(configured: Option<&str>) -> Result<PathBuf, ToolError> {
    if let Some(path) = configured {
        return Ok(PathBuf::from(path));
    }
    if let Ok(path) = std::env::var("SWAGE_BROWSER_BIN") {
        return Ok(PathBuf::from(path));
    }
    for candidate in ["chromium", "google-chrome-stable", "chromium-browser"] {
        if let Some(path) = find_on_path(candidate) {
            return Ok(path);
        }
    }
    Err(ToolError::ExecutionFailed {
        name: "browser".to_string(),
        message: "no Chromium binary found; set [tools.browser].binary_path or SWAGE_BROWSER_BIN"
            .to_string(),
    })
}

fn find_on_path(binary: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|path| path.join(binary))
            .find(|path| path.is_file())
    })
}

fn reserve_local_port() -> Result<u16, ToolError> {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.local_addr())
        .map(|addr| addr.port())
        .map_err(|error| ToolError::ExecutionFailed {
            name: "browser".to_string(),
            message: format!("failed to reserve DevTools port: {error}"),
        })
}

async fn wait_for_devtools(endpoint: &str) -> Result<(), ToolError> {
    let client = reqwest::Client::new();
    let version_url = format!("{endpoint}/json/version");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(response) = client.get(&version_url).send().await
            && response.status().is_success()
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(ToolError::ExecutionFailed {
                name: "browser".to_string(),
                message: "Chromium did not open its DevTools endpoint within 10s".to_string(),
            });
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn resolve_object(
    page: &Page,
    backend_node_id: BackendNodeId,
    tool_name: &str,
) -> Result<chromiumoxide::cdp::js_protocol::runtime::RemoteObjectId, ToolError> {
    page.execute(
        ResolveNodeParams::builder()
            .backend_node_id(backend_node_id)
            .build(),
    )
    .await
    .map_err(browser_error(
        tool_name,
        "that element is no longer on the page — call browser_snapshot for the current state",
    ))?
    .result
    .object
    .object_id
    .ok_or_else(|| ToolError::ExecutionFailed {
        name: tool_name.to_string(),
        message:
            "that element is no longer on the page — call browser_snapshot for the current state"
                .to_string(),
    })
}

async fn query_accessibility_tree(page: &Page) -> Result<Vec<AxNode>, ToolError> {
    let document = page
        .execute(GetDocumentParams::builder().depth(-1).pierce(true).build())
        .await
        .map_err(browser_error(
            "browser_snapshot",
            "failed to read document node",
        ))?
        .result
        .root;
    page.execute(
        QueryAxTreeParams::builder()
            .node_id(document.node_id)
            .build(),
    )
    .await
    .map(|response| response.result.nodes)
    .map_err(browser_error(
        "browser_snapshot",
        "failed to query accessibility tree",
    ))
}

#[derive(Debug, Deserialize)]
struct DomNodeSnapshot {
    role: String,
    name: String,
    selector: String,
    state: Vec<String>,
}

async fn dom_snapshot(
    page: &Page,
    url: &str,
    title: &str,
    snapshot_seq: u64,
    max_bytes: usize,
    masked_selectors: &HashSet<String>,
) -> Result<RenderedSnapshot, ToolError> {
    let masked_selectors_json =
        serde_json::to_string(masked_selectors).map_err(|error| ToolError::ExecutionFailed {
            name: "browser_snapshot".to_string(),
            message: format!("failed to encode browser mask selectors: {error}"),
        })?;
    let nodes: Vec<DomNodeSnapshot> = page
        .evaluate_function(
            r#"() => {
                const maskedSelectors = new Set(__MASKED_SELECTORS__);
                const cssPath = (el) => {
                    if (el.id) return '#' + CSS.escape(el.id);
                    const parts = [];
                    for (let node = el; node && node.nodeType === Node.ELEMENT_NODE && node !== document.documentElement; node = node.parentElement) {
                        const siblings = Array.from(node.parentElement ? node.parentElement.children : []).filter(s => s.tagName === node.tagName);
                        const index = siblings.indexOf(node) + 1;
                        parts.unshift(node.tagName.toLowerCase() + (siblings.length > 1 ? `:nth-of-type(${index})` : ''));
                    }
                    return parts.join(' > ');
                };
                const labelFor = (el) => {
                    if (el.getAttribute('aria-label')) return el.getAttribute('aria-label');
                    if (el.id) {
                        const label = document.querySelector(`label[for="${CSS.escape(el.id)}"]`);
                        if (label) return label.innerText.trim();
                    }
                    const wrapping = el.closest('label');
                    if (wrapping) return wrapping.innerText.trim();
                    return (el.innerText || el.getAttribute('aria-placeholder') || el.getAttribute('placeholder') || el.getAttribute('title') || el.getAttribute('alt') || el.getAttribute('name') || el.id || '').trim();
                };
                const roleFor = (el) => {
                    if (el.getAttribute('role')) return el.getAttribute('role');
                    const tag = el.tagName.toLowerCase();
                    if (tag === 'a') return 'link';
                    if (tag === 'button') return 'button';
                    if (tag === 'select') return 'combobox';
                    if (tag === 'textarea') return 'textbox';
                    if (tag === 'input') {
                        const type = (el.getAttribute('type') || 'text').toLowerCase();
                        if (type === 'checkbox') return 'checkbox';
                        if (type === 'radio') return 'radio';
                        if (type === 'submit' || type === 'button') return 'button';
                        return 'textbox';
                    }
                    if (/^h[1-6]$/.test(tag)) return 'heading';
                    return '';
                };
                return Array.from(document.querySelectorAll('h1,h2,h3,h4,h5,h6,a,button,input,textarea,select,[role],[tabindex]'))
                    .map(el => {
                        const role = roleFor(el);
                        const state = [];
                        const selector = cssPath(el);
                        const inputType = (el.getAttribute('type') || '').toLowerCase();
                        const maskValue = inputType === 'password' || maskedSelectors.has(selector);
                        if (el.disabled) state.push('disabled');
                        if (el.checked) state.push('checked');
                        if (el.required) state.push('required');
                        if (el.value && role === 'textbox') {
                            state.push(maskValue ? 'value="[masked]"' : `value="${el.value}"`);
                        }
                        return {role, name: labelFor(el), selector, state};
                    })
                    .filter(node => node.role);
            }"#
            .replace("__MASKED_SELECTORS__", &masked_selectors_json),
        )
        .await
        .map_err(browser_error("browser_snapshot", "failed to build DOM semantic fallback"))?
        .into_value()
        .map_err(|error| ToolError::ExecutionFailed {
            name: "browser_snapshot".to_string(),
            message: format!("failed to decode DOM semantic fallback: {error}"),
        })?;
    let mut refs = HashMap::new();
    let mut lines = vec![
        format!("url: {url}"),
        format!("title: {title}"),
        format!("snapshot: {snapshot_seq}"),
        String::new(),
    ];
    for (index, node) in nodes.into_iter().enumerate() {
        let reference = format!("e{}", index + 1);
        let state = if node.state.is_empty() {
            String::new()
        } else {
            format!(" [{}]", node.state.join(", "))
        };
        lines.push(format!(
            "- {} \"{}\"{} ref={}",
            node.role,
            node.name.replace('"', "\\\""),
            state,
            reference
        ));
        refs.insert(
            reference.clone(),
            RefEntry {
                reference,
                backend_node_id: BackendNodeId::new(0),
                selector: Some(node.selector),
                role: node.role,
                name: node.name,
            },
        );
    }
    let full = lines.join("\n");
    let output = if full.len() > max_bytes {
        full.chars().take(max_bytes).collect::<String>()
    } else {
        full.clone()
    };
    Ok(RenderedSnapshot {
        full_output: (output.len() < full.len()).then_some(full),
        output,
        refs,
    })
}

fn parse_browser_url(raw: &str) -> Result<Url, ToolError> {
    Url::parse(raw).map_err(|error| ToolError::InvalidParameters {
        name: "browser_navigate".to_string(),
        message: format!("invalid url: {error}"),
    })
}

async fn current_url(page: &Page) -> Result<String, ToolError> {
    page.url()
        .await
        .map_err(browser_error(
            "browser_snapshot",
            "failed to read current URL",
        ))?
        .ok_or_else(|| ToolError::ExecutionFailed {
            name: "browser_snapshot".to_string(),
            message: "the current page has no URL".to_string(),
        })
}

async fn current_title(page: &Page) -> Result<String, ToolError> {
    page.evaluate("document.title")
        .await
        .map_err(browser_error(
            "browser_snapshot",
            "failed to read page title",
        ))?
        .into_value::<String>()
        .map_err(|error| ToolError::ExecutionFailed {
            name: "browser_snapshot".to_string(),
            message: format!("failed to decode page title: {error}"),
        })
}

fn stale_ref_error(tool_name: &str) -> ToolError {
    ToolError::ExecutionFailed {
        name: tool_name.to_string(),
        message:
            "the page has navigated since that snapshot — use the refs from the latest snapshot"
                .to_string(),
    }
}

fn browser_error(
    tool_name: &str,
    context: &'static str,
) -> impl Fn(chromiumoxide::error::CdpError) -> ToolError {
    let tool_name = tool_name.to_string();
    move |error| ToolError::ExecutionFailed {
        name: tool_name.clone(),
        message: format!("{context}: {error}"),
    }
}
