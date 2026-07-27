//! Engine runtime embedded in the kitty process.
//!
//! One `EngineRuntime` per process: a small multi-thread tokio runtime plus a
//! per-window session table. Each window gets its own conversation database
//! (state_dir/windows/<id>) so sessions are fully isolated, while config,
//! knowledge base and memory remain shared.

use crate::agent::{Agent, AgentEvent, AgentMode};
use crate::config::AppConfig;
use crate::ffi::context::{ContextStore, LineKind};
use crate::ffi::output::OutputQueue;
use crate::llm::OpenAiCompatibleClient;
use crate::paths::MossPaths;
use crate::state::StateStore;
use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tracing::{error, info, warn};

/// Ring capacity per window: lines / bytes.
const CTX_MAX_LINES: usize = 2000;
const CTX_MAX_BYTES: usize = 512 * 1024;
/// Byte budget for the terminal-context block of a single ask (~8k tokens).
const CTX_ASK_BUDGET: usize = 24 * 1024;

pub struct Session {
    pub window_id: u64,
    pub ctx: Mutex<ContextStore>,
    pub cwd: Mutex<Option<String>>,
    pub agent: tokio::sync::Mutex<Option<Agent>>,
    pub busy: AtomicBool,
    /// Capture mute: set for the whole ask→drain window. `busy` alone is not
    /// enough — the kitty side keeps injecting answer bytes (which re-enter
    /// via on_line) AFTER the turn future finishes; only the injector knows
    /// when the last byte hit the screen, so it unmutes via moss_set_capture.
    pub capture_muted: AtomicBool,
    pub cancel: AtomicBool,
    pub out: Arc<OutputQueue>,
}

impl Session {
    fn new(window_id: u64) -> Self {
        Self {
            window_id,
            ctx: Mutex::new(ContextStore::new(CTX_MAX_LINES, CTX_MAX_BYTES)),
            cwd: Mutex::new(None),
            agent: tokio::sync::Mutex::new(None),
            busy: AtomicBool::new(false),
            capture_muted: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
            out: Arc::new(OutputQueue::new()),
        }
    }
}

pub struct EngineRuntime {
    pub runtime: tokio::runtime::Runtime,
    pub config: RwLock<Arc<AppConfig>>,
    pub paths: MossPaths,
    sessions: Mutex<HashMap<u64, Arc<Session>>>,
    /// True when no user config.jsonc existed at init: we are answering via
    /// the built-in zero-key public provider, which the user must be told
    /// about once (their terminal context leaves the machine).
    default_provider_notice_pending: std::sync::atomic::AtomicBool,
    _log_guard: Option<crate::logging::LoggingGuard>,
}

/// Build MossPaths, honouring a MOSS_HOME override so an embedded engine can
/// be fully sandboxed without touching the host account's real XDG tree.
fn resolve_paths() -> Result<MossPaths> {
    if let Some(root) = std::env::var_os("MOSS_HOME") {
        let root = PathBuf::from(root);
        let config_dir = root.join("config");
        Ok(MossPaths {
            config_file: config_dir.join("config.jsonc"),
            skills_dir: config_dir.join("skills"),
            fish_hook_file: config_dir.join("fish/conf.d/moss.fish"),
            bash_hook_file: config_dir.join("shell/bash-hook.sh"),
            zsh_hook_file: config_dir.join("shell/zsh-hook.zsh"),
            scripts_dir: config_dir.join("scripts"),
            system_scripts_dir: root.join("system-scripts"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            state_dir: root.join("state"),
            pictures_dir: root.join("pictures"),
            config_dir,
        })
    } else {
        MossPaths::new()
    }
}

/// Per-window isolation: conversation state AND memory live under
/// windows/<id> so nothing recalled in one kitty window can leak into
/// another (US-008). The knowledge base intentionally stays shared — see
/// build_agent, which pins it back to the base data dir.
fn window_paths(base: &MossPaths, window_id: u64) -> MossPaths {
    let mut p = base.clone();
    p.state_dir = base.state_dir.join("windows").join(window_id.to_string());
    p.data_dir = base.data_dir.join("windows").join(window_id.to_string());
    p
}

/// Context-window fallback: if neither the user config nor the models cache
/// can resolve a window for an active model, pin a conservative default so
/// the trim/compact stack never silently disengages (FR-004). Must be called
/// off the tokio runtime — the resolution path may hit a blocking client.
fn apply_context_window_fallback(config: &mut AppConfig) {
    const FALLBACK_CONTEXT_WINDOW: usize = 65_536;
    for choice in config.active_provider_model_choices() {
        let resolved = config
            .context_window_for_provider_model(&choice.provider_id, &choice.model)
            .ok()
            .flatten();
        if resolved.is_some() {
            continue;
        }
        if let Some(provider) = config
            .providers
            .iter_mut()
            .find(|p| p.id == choice.provider_id)
        {
            warn!(
                "no context window known for {}/{}; assuming {FALLBACK_CONTEXT_WINDOW} tokens",
                choice.provider_id, choice.model
            );
            provider
                .model_context_window
                .insert(choice.model.clone(), FALLBACK_CONTEXT_WINDOW);
        }
    }
}

fn build_agent(config: &AppConfig, base_paths: &MossPaths, paths: &MossPaths) -> Result<Agent> {
    std::fs::create_dir_all(&paths.state_dir).with_context(|| {
        format!(
            "creating per-window state dir {}",
            paths.state_dir.display()
        )
    })?;
    let mut config = config.clone();
    if config.plugins.knowledge_base.data_dir.trim().is_empty() {
        // Keep the knowledge base shared across windows even though the
        // rest of data_dir is sharded per window.
        config.plugins.knowledge_base.data_dir =
            base_paths.data_dir.join("kb").display().to_string();
    }
    let client = OpenAiCompatibleClient::from_config(&config, paths)?;
    let tools = crate::tools::builtin_registry(&config, paths);
    let state = StateStore::new(paths)?;
    Agent::new(config, paths, state, client, tools, AgentMode::Normal)
}

impl EngineRuntime {
    pub fn create() -> Result<Arc<Self>> {
        let paths = resolve_paths()?;
        paths.create_dirs()?;
        let language = AppConfig::display_language_hint(&paths);
        crate::i18n::init(language.as_deref().unwrap_or("auto"));
        let log_guard = match crate::logging::init(&paths, false) {
            Ok(guard) => Some(guard),
            Err(err) => {
                eprintln!("moss: logging unavailable: {err:#}");
                None
            }
        };
        let user_config_exists = paths.config_file.exists();
        let config = AppConfig::load_or_default(&paths)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("moss-engine")
            .enable_all()
            .build()
            .context("building moss tokio runtime")?;
        // Load the model-metadata cache (context windows etc.). Without it
        // the whole trim/compact stack silently no-ops for providers whose
        // window is not hand-configured — the CLI does this in cli.rs, the
        // embedded path must do it itself. Both calls are sync/off-runtime:
        // the refresh runs on its own std::thread with a blocking client, so
        // it must never be entered from a tokio worker.
        crate::models_cache::try_load(&paths);
        crate::models_cache::spawn_background_refresh(paths.clone());
        let mut config = config;
        apply_context_window_fallback(&mut config);
        info!("moss engine runtime initialized (embedded)");
        Ok(Arc::new(Self {
            runtime,
            config: RwLock::new(Arc::new(config)),
            paths,
            sessions: Mutex::new(HashMap::new()),
            default_provider_notice_pending: std::sync::atomic::AtomicBool::new(
                !user_config_exists,
            ),
            _log_guard: log_guard,
        }))
    }

    pub fn session(&self, window_id: u64) -> Arc<Session> {
        let mut map = self.sessions.lock().unwrap();
        map.entry(window_id)
            .or_insert_with(|| Arc::new(Session::new(window_id)))
            .clone()
    }

    /// Look up without creating (poll paths must not allocate sessions).
    pub fn existing_session(&self, window_id: u64) -> Option<Arc<Session>> {
        self.sessions.lock().unwrap().get(&window_id).cloned()
    }

    pub fn on_line(&self, window_id: u64, kind: u8, text: &str) {
        let session = self.session(window_id);
        // While an answer is streaming, every new screen line is the
        // engine's own injected output; capturing it would feed the answer
        // back into the next ask's context. The reply already lives in the
        // conversation history, so drop these lines.
        if session.busy.load(Ordering::SeqCst) || session.capture_muted.load(Ordering::SeqCst) {
            return;
        }
        session
            .ctx
            .lock()
            .unwrap()
            .push(LineKind::from_u8(kind), text);
    }

    pub fn on_prompt_mark(&self, window_id: u64, mark: u8, data: &str) {
        let session = self.session(window_id);
        if mark == b'D' {
            let status: i64 = data.trim().parse().unwrap_or(0);
            session.ctx.lock().unwrap().push_exit_status(status);
        }
    }

    pub fn on_cwd(&self, window_id: u64, cwd: &str) {
        let session = self.session(window_id);
        *session.cwd.lock().unwrap() = Some(parse_osc7_cwd(cwd));
    }

    pub fn set_capture(&self, window_id: u64, enabled: bool) {
        if let Some(session) = self.existing_session(window_id) {
            session.capture_muted.store(!enabled, Ordering::SeqCst);
        }
    }

    pub fn cancel(&self, window_id: u64) {
        if let Some(session) = self.existing_session(window_id) {
            session.cancel.store(true, Ordering::SeqCst);
        }
    }

    pub fn window_closed(&self, window_id: u64) {
        if let Some(session) = self.sessions.lock().unwrap().remove(&window_id) {
            session.cancel.store(true, Ordering::SeqCst);
            info!("moss session for window {window_id} closed");
        }
    }

    pub fn reload_config(&self) -> Result<()> {
        let mut fresh = AppConfig::load_or_default(&self.paths)?;
        apply_context_window_fallback(&mut fresh);
        *self.config.write().unwrap() = Arc::new(fresh);
        // Drop idle agents so the next ask rebuilds them with the new config.
        let sessions: Vec<Arc<Session>> = self.sessions.lock().unwrap().values().cloned().collect();
        for session in sessions {
            if !session.busy.load(Ordering::SeqCst) {
                if let Ok(mut guard) = session.agent.try_lock() {
                    *guard = None;
                }
            }
        }
        Ok(())
    }

    /// Start an ask for a window. Returns 0 on success, -2 if a request is
    /// already streaming for this window.
    pub fn ask(self: &Arc<Self>, window_id: u64, question: &str) -> i32 {
        let question = question
            .trim_start()
            .trim_start_matches(crate::ffi::context::TRIGGER_PREFIX)
            .trim()
            .to_string();
        if question.is_empty() {
            return -3;
        }
        let session = self.session(window_id);
        if session
            .busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return -2;
        }
        session.cancel.store(false, Ordering::SeqCst);
        session.out.begin();
        if self
            .default_provider_notice_pending
            .swap(false, Ordering::SeqCst)
        {
            session.out.push_notice(
                "[未检测到用户配置：本次由内置 opencode Zen 公共端点回答，终端上下文将发送至 opencode.ai。运行 moss-setup 或 moss config 配置自有 provider]",
            );
        }
        // Muted until the kitty side reports the answer fully injected
        // (moss_set_capture) — prevents the answer leaking into the next
        // increment. The context below was collected before this point.
        session.capture_muted.store(true, Ordering::SeqCst);

        let (context_block, cwd) = {
            let mut ctx = session.ctx.lock().unwrap();
            let block = ctx.collect_and_advance(CTX_ASK_BUDGET);
            let cwd = session.cwd.lock().unwrap().clone();
            (block, cwd)
        };
        let mut input = String::new();
        if !context_block.trim().is_empty() {
            input.push_str("【终端上下文】\n");
            input.push_str(&context_block);
        }
        if let Some(cwd) = cwd {
            input.push_str(&format!("【工作目录】{cwd}\n"));
        }
        input.push_str("【问题】\n");
        input.push_str(&question);

        let engine = Arc::clone(self);
        let task_session = Arc::clone(&session);
        self.runtime.spawn(async move {
            run_turn(engine, task_session, input).await;
        });
        0
    }
}

/// OSC 7 delivers a "file://host/path" URL (possibly percent-encoded);
/// reduce it to a plain filesystem path. Non-URL input passes through.
fn parse_osc7_cwd(raw: &str) -> String {
    let path = match raw.strip_prefix("file://") {
        Some(rest) => match rest.find('/') {
            Some(idx) => &rest[idx..],
            None => "/",
        },
        None => raw,
    };
    // Minimal %XX decoding (UTF-8 lossy on invalid sequences).
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            ) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn run_turn(engine: Arc<EngineRuntime>, session: Arc<Session>, input: String) {
    let out = Arc::clone(&session.out);
    let result = run_turn_inner(&engine, &session, &input).await;
    if let Err(err) = result {
        if err.to_string().contains("cancelled by user") {
            out.push_notice("[interrupted]");
        } else {
            error!("moss turn failed: {err:#}");
            out.push_error(&format!("{err:#}"));
        }
    }
    out.finish();
    session.busy.store(false, Ordering::SeqCst);
}

async fn run_turn_inner(
    engine: &Arc<EngineRuntime>,
    session: &Arc<Session>,
    input: &str,
) -> Result<()> {
    let mut agent_guard = session.agent.lock().await;
    if agent_guard.is_none() {
        let config = engine.config.read().unwrap().clone();
        let paths = window_paths(&engine.paths, session.window_id);
        match build_agent(&config, &engine.paths, &paths) {
            Ok(agent) => *agent_guard = Some(agent),
            Err(err) => {
                return Err(anyhow!(
                    "{err:#}\n{}",
                    crate::i18n::text(
                        "hint: run `moss config` (or moss-setup) to configure an LLM provider",
                        "提示：运行 `moss config`（或 moss-setup）配置 LLM provider"
                    )
                ));
            }
        }
    }
    let agent = agent_guard.as_mut().unwrap();

    let out = Arc::clone(&session.out);
    let cancel_flag = Arc::clone(session).cancel_watcher();
    let mut result = agent
        .chat_stream(input, |event| {
            if cancel_flag.load(Ordering::SeqCst) {
                anyhow::bail!("cancelled by user");
            }
            match &event {
                AgentEvent::PrepareForExternalOutput { .. } | AgentEvent::AskQuestion { .. } => {}
                other => out.push_event(other),
            }
            // Consume responder-carrying events so the agent never stalls.
            match event {
                AgentEvent::PrepareForExternalOutput { ready } => {
                    let _ = ready.send(());
                }
                AgentEvent::AskQuestion { responder, .. } => {
                    // ask_question is not registered for embedded sessions;
                    // if it ever fires, cancel it cleanly.
                    let _ = responder.send(crate::question::QuestionResponse::Cancelled);
                }
                _ => {}
            }
            Ok(())
        })
        .await;

    // Force tier of the FR-004 ladder: when the token-estimate ladder missed
    // and the provider itself rejected the request as over-window, compact
    // the history and retry once instead of surfacing a raw API error.
    if let Err(err) = &result {
        if crate::llm::is_context_overflow_error(err) && !cancel_flag.load(Ordering::SeqCst) {
            warn!("provider rejected request as over-window; forcing compaction and retrying");
            out.push_notice("[context overflow: compacting and retrying...]");
            let compacted = agent.compact_now(|_event| Ok(())).await;
            match compacted {
                Ok(Some(_)) | Ok(None) => {
                    result = agent
                        .chat_stream(input, |event| {
                            if cancel_flag.load(Ordering::SeqCst) {
                                anyhow::bail!("cancelled by user");
                            }
                            match &event {
                                AgentEvent::PrepareForExternalOutput { .. }
                                | AgentEvent::AskQuestion { .. } => {}
                                other => out.push_event(other),
                            }
                            match event {
                                AgentEvent::PrepareForExternalOutput { ready } => {
                                    let _ = ready.send(());
                                }
                                AgentEvent::AskQuestion { responder, .. } => {
                                    let _ = responder
                                        .send(crate::question::QuestionResponse::Cancelled);
                                }
                                _ => {}
                            }
                            Ok(())
                        })
                        .await;
                }
                Err(compact_err) => {
                    warn!("forced compaction failed: {compact_err:#}");
                }
            }
        }
    }

    match result {
        Ok(_chat) => {
            if let Ok(context_tokens) = agent.effective_context_tokens() {
                if let Err(err) = agent
                    .handle_overflow_after_turn(context_tokens, |_event| Ok(()))
                    .await
                {
                    warn!("post-turn overflow handling failed: {err:#}");
                }
            }
            Ok(())
        }
        Err(err) => Err(err),
    }
}

impl Session {
    fn cancel_watcher(self: Arc<Self>) -> Arc<CancelFlag> {
        Arc::new(CancelFlag(self))
    }
}

/// Thin view over Session exposing only the cancel flag to the callback.
pub struct CancelFlag(Arc<Session>);

impl CancelFlag {
    pub fn load(&self, order: Ordering) -> bool {
        self.0.cancel.load(order)
    }
}
