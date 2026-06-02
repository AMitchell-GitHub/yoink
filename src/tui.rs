//! Native TUI for yoink. Replaces the previous fzf-as-frontend design with an
//! in-process ratatui app — instant inline toggles, always-visible status,
//! overlays that don't dismiss the search.
//!
//! The UI is written to **stderr** so that the controlling tty receives the
//! rendering and **stdout** stays clean for the cd-via-stdout shell wrapper
//! contract (yoink prints exactly the target dir to stdout when the user
//! triggers the `cd` action, and nothing else).

use crate::actions::{self, Action};
use crate::blame;
use crate::keys::{builtin, KeyBind};
use crate::search::{
    build_blame_sorted_entries, build_search_entries, config_path, load_settings, SearchEntry,
    SearchMode, Sort, YoinkSettings,
};
use crate::settings::write_setting;
use ansi_to_tui::IntoText;
use anyhow::{Context, Result};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::io::{stderr, Stderr, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

type Tui = Terminal<CrosstermBackend<Stderr>>;

/// Top-level entry point invoked by main.rs after dependency checks.
pub fn run_session(initial_query: Option<&str>, cwd: &Path) -> Result<()> {
    ensure_config_exists();

    // Per-session blame cache lives in the same /tmp area the old fzf path
    // used so any subprocess we shell out to (e.g. preview's bat) sees it.
    let cache_dir = std::env::temp_dir().join(format!("yoink-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache_dir);
    std::env::set_var(blame::SESSION_CACHE_ENV, &cache_dir);

    install_panic_hook();

    // Enter the TUI behind a Drop guard so the terminal is ALWAYS restored
    // — even if `App::new` fails (e.g. config parse error), even if `App::run`
    // returns an error, even on panic. Without this the user gets stuck in
    // raw mode + alt screen + mouse capture and can't `ctrl-c` out.
    let mut terminal = enter_tui()?;
    let _restore = TuiRestore;

    let outcome = App::new(cwd, initial_query).and_then(|app| app.run(&mut terminal));

    // `_restore` drops here on the way out, restoring the terminal before we
    // either print the cd target or propagate the error.
    drop(_restore);

    let _ = std::fs::remove_dir_all(&cache_dir);

    match outcome {
        Ok(Some(Outcome::Cd(path))) => {
            println!("{}", path.display());
            Ok(())
        }
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Drop guard that disables raw mode and exits the alternate screen +
/// mouse capture on the way out. Constructed *after* successful
/// `enter_tui()`, so we never accidentally try to leave a TUI we never
/// entered.
struct TuiRestore;

impl Drop for TuiRestore {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(stderr(), LeaveAlternateScreen, DisableMouseCapture);
    }
}

fn enter_tui() -> Result<Tui> {
    enable_raw_mode().context("failed to enable raw mode")?;
    let mut err = stderr();
    execute!(err, EnterAlternateScreen, EnableMouseCapture)
        .context("failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(err);
    Terminal::new(backend).context("failed to construct terminal")
}

#[allow(dead_code)]
fn leave_tui(terminal: &mut Tui) -> Result<()> {
    // Superseded by TuiRestore (Drop guard). Kept around for completeness;
    // callers should rely on the Drop path so the restore happens on error
    // paths too.
    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture).ok();
    terminal.show_cursor().ok();
    Ok(())
}

/// Hand the tty back to a foreground subprocess (vim/nano/...), then re-enter
/// the TUI when it returns. Same dance crossterm-based apps standardly do.
fn suspend_tui_for<F, T>(terminal: &mut Tui, f: F) -> Result<T>
where
    F: FnOnce() -> T,
{
    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture).ok();
    let result = f();
    enable_raw_mode().ok();
    execute!(terminal.backend_mut(), EnterAlternateScreen, EnableMouseCapture).ok();
    terminal.clear().ok();
    Ok(result)
}

/// On first interactive launch, materialize the fully-annotated reference
/// config to the user's home so they have a discoverable, self-documenting
/// starting point. After this point we never overwrite — every edit is
/// the user's own (either by hand or via the inline Alt-* toggles).
fn ensure_config_exists() {
    let Some(path) = crate::search::config_path() else {
        return;
    };
    if path.exists() {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, DEFAULT_CONFIG_REFERENCE);
}

/// A panic in the middle of the TUI must still restore the terminal,
/// otherwise the user is left in a broken alt-screen / raw-mode session.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(stderr(), LeaveAlternateScreen, DisableMouseCapture);
        default(info);
    }));
}

#[derive(Debug)]
enum Outcome {
    Cd(PathBuf),
}

#[derive(Debug, Clone)]
enum SearchProgress {
    Started,
    Done(Vec<SearchEntry>),
    Failed(String),
}

#[derive(Debug, Clone)]
enum PreviewProgress {
    Done {
        key: PreviewKey,
        body: String,
    },
    Failed {
        key: PreviewKey,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PreviewKey {
    path: PathBuf,
    line: Option<usize>,
}

/// Messages worker threads push to the main event loop. Terminal events are
/// **not** in this enum on purpose — they're polled in the main thread (see
/// `App::run`) so that while `suspend_tui_for` is blocked inside an editor's
/// `wait()` we never read stdin and the editor receives every key.
#[derive(Debug)]
enum AppEvent {
    Search(SearchProgress),
    Preview(PreviewProgress),
    BlameProgress {
        done: usize,
        total: usize,
        current: String,
    },
    BlameDone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Overlay {
    None,
    Help,
    Settings,
    QuickPick(QuickPickKind),
    /// One-time keybind-conflict warning shown at startup. Dismissed by any key.
    Warning,
}

/// Working copy of the persisted defaults, edited inside the Settings overlay.
/// `saved_*` is the on-disk baseline captured when the overlay opened; the
/// draft is "dirty" (and Save becomes selectable) when it diverges.
#[derive(Debug, Clone, Copy)]
struct SettingsDraft {
    mode: SearchMode,
    case: bool,
    sort: Sort,
    saved_mode: SearchMode,
    saved_case: bool,
    saved_sort: Sort,
}

impl SettingsDraft {
    fn from_settings(s: &YoinkSettings) -> Self {
        SettingsDraft {
            mode: s.search_mode,
            case: s.case_sensitive,
            sort: s.sort,
            saved_mode: s.search_mode,
            saved_case: s.case_sensitive,
            saved_sort: s.sort,
        }
    }

    fn dirty(&self) -> bool {
        self.mode != self.saved_mode
            || self.case != self.saved_case
            || self.sort != self.saved_sort
    }
}

/// Which inline session toggle the user is choosing from. Each kind has a
/// fixed list of options; cursor opens on the current setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuickPickKind {
    Mode,
    Case,
    Sort,
}

impl QuickPickKind {
    fn title(self) -> &'static str {
        match self {
            QuickPickKind::Mode => "Search mode (session)",
            QuickPickKind::Case => "Case sensitivity (session)",
            QuickPickKind::Sort => "Sort order (session)",
        }
    }

    /// Display labels for each option, in their canonical order.
    fn labels(self) -> &'static [&'static str] {
        match self {
            QuickPickKind::Mode => &["glob", "regex"],
            QuickPickKind::Case => &["insensitive", "sensitive"],
            QuickPickKind::Sort => &[
                "depth",
                "alphabetical",
                "blame_young  (youngest blame first)",
                "blame_old    (oldest blame first)",
            ],
        }
    }

    fn current_index(self, settings: &YoinkSettings) -> usize {
        match self {
            QuickPickKind::Mode => match settings.search_mode {
                SearchMode::Glob => 0,
                SearchMode::Regex => 1,
            },
            QuickPickKind::Case => {
                if settings.case_sensitive {
                    1
                } else {
                    0
                }
            }
            QuickPickKind::Sort => match settings.sort {
                Sort::Depth => 0,
                Sort::Alphabetical => 1,
                Sort::BlameYoung => 2,
                Sort::BlameOld => 3,
            },
        }
    }
}

struct App {
    cwd: PathBuf,
    settings: YoinkSettings,
    query: String,
    cursor: usize,
    entries: Vec<SearchEntry>,
    selection: ListState,
    list_visible_rows: usize,
    searching: bool,
    last_error: Option<String>,
    flash: Option<(String, std::time::Instant)>,
    overlay: Overlay,
    settings_cursor: usize,
    settings_draft: SettingsDraft,
    /// Keybind conflicts detected at startup, shown once via `Overlay::Warning`.
    startup_warnings: Vec<String>,
    quick_pick_cursor: usize,
    preview: Option<PreviewKey>,
    preview_body: Option<Text<'static>>,
    preview_loading: bool,
    pending_open: Option<Action>,
    blame_progress: Option<(usize, usize, String)>,
    events_rx: Receiver<AppEvent>,
    events_tx: Sender<AppEvent>,
    quit: bool,
    outcome: Option<Outcome>,
}

impl App {
    fn new(cwd: &Path, initial_query: Option<&str>) -> Result<Self> {
        let settings = load_settings()?;
        let (events_tx, events_rx) = mpsc::channel();

        let mut selection = ListState::default();
        selection.select(Some(0));

        let startup_warnings = detect_key_conflicts(&settings);
        let initial_overlay = if startup_warnings.is_empty() {
            Overlay::None
        } else {
            Overlay::Warning
        };
        let settings_draft = SettingsDraft::from_settings(&settings);

        let mut app = App {
            cwd: cwd.to_path_buf(),
            settings,
            query: initial_query.unwrap_or("").to_string(),
            cursor: initial_query.map(|q| q.chars().count()).unwrap_or(0),
            entries: Vec::new(),
            selection,
            list_visible_rows: 0,
            searching: false,
            last_error: None,
            flash: None,
            overlay: initial_overlay,
            settings_cursor: 0,
            settings_draft,
            startup_warnings,
            quick_pick_cursor: 0,
            preview: None,
            preview_body: None,
            preview_loading: false,
            pending_open: None,
            blame_progress: None,
            events_rx,
            events_tx,
            quit: false,
            outcome: None,
        };
        if !app.query.is_empty() {
            app.run_search();
        }
        Ok(app)
    }

    fn run(mut self, terminal: &mut Tui) -> Result<Option<Outcome>> {
        while !self.quit {
            terminal.draw(|f| self.draw(f))?;

            // Poll crossterm directly. While we're inside `suspend_tui_for`
            // (waiting on an editor child to exit), this main thread is
            // blocked there — so stdin is *not* being read by us and the
            // editor receives every keystroke. That's the whole reason this
            // runs on the main thread rather than a background poller.
            if event::poll(Duration::from_millis(50))? {
                match event::read()? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        self.handle_key(key, terminal)?;
                    }
                    Event::Mouse(m) => self.handle_mouse(m),
                    Event::Resize(_, _) => {}
                    _ => {}
                }
            }

            // Drain worker messages (search/preview/blame progress) without
            // blocking. These arrive from background threads.
            loop {
                match self.events_rx.try_recv() {
                    Ok(msg) => self.handle_worker_event(msg)?,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.quit = true;
                        break;
                    }
                }
            }

            if let Some((_, when)) = &self.flash {
                if when.elapsed() > Duration::from_millis(1600) {
                    self.flash = None;
                }
            }
        }
        Ok(self.outcome)
    }

    fn handle_worker_event(&mut self, event: AppEvent) -> Result<()> {
        match event {
            AppEvent::Search(SearchProgress::Started) => {
                self.searching = true;
            }
            AppEvent::Search(SearchProgress::Done(entries)) => {
                self.searching = false;
                self.entries = entries;
                if self.entries.is_empty() {
                    self.selection.select(None);
                    self.preview = None;
                    self.preview_body = None;
                } else {
                    self.selection.select(Some(0));
                    self.queue_preview();
                }
                self.last_error = None;
            }
            AppEvent::Search(SearchProgress::Failed(msg)) => {
                self.searching = false;
                self.last_error = Some(msg);
                self.entries.clear();
                self.preview = None;
                self.preview_body = None;
            }
            AppEvent::Preview(PreviewProgress::Done { key, body }) => {
                if self.preview.as_ref() == Some(&key) {
                    self.preview_loading = false;
                    self.preview_body = Some(ansi_to_text(&body));
                }
            }
            AppEvent::Preview(PreviewProgress::Failed { key, message }) => {
                if self.preview.as_ref() == Some(&key) {
                    self.preview_loading = false;
                    self.preview_body = Some(Text::from(message));
                }
            }
            AppEvent::BlameProgress {
                done,
                total,
                current,
            } => {
                self.blame_progress = Some((done, total, current));
            }
            AppEvent::BlameDone => {
                self.blame_progress = None;
                self.run_search();
            }
        }
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent, terminal: &mut Tui) -> Result<()> {
        // Dismiss overlays on Esc.
        if matches!(key.code, KeyCode::Esc) && self.overlay != Overlay::None {
            self.overlay = Overlay::None;
            return Ok(());
        }

        // Quit: Ctrl-C anywhere.
        if builtin::QUIT.matches(key) {
            self.quit = true;
            return Ok(());
        }

        // The startup keybind-conflict warning is dismissed by any key (Esc
        // and Ctrl-C are handled above). Swallow the key so it doesn't also
        // act on the search behind it.
        if self.overlay == Overlay::Warning {
            self.overlay = Overlay::None;
            return Ok(());
        }

        // Always-on bindings (must precede the query-input path so they fire
        // even when the user is typing).
        //
        // Help is F1 only — `?` is intentionally NOT bound (see keys::builtin).
        // It's both a glob wildcard and part of regex inline flags like
        // `(?i)`, so it must flow through to the query input.
        if builtin::HELP.matches(key) {
            self.overlay = if self.overlay == Overlay::Help {
                Overlay::None
            } else {
                Overlay::Help
            };
            return Ok(());
        }
        if builtin::SETTINGS.matches(key) {
            if self.overlay == Overlay::Settings {
                self.overlay = Overlay::None;
            } else {
                self.open_settings();
            }
            return Ok(());
        }
        if builtin::MODE.matches(key) {
            self.open_quick_pick(QuickPickKind::Mode);
            return Ok(());
        }
        if builtin::CASE.matches(key) {
            self.open_quick_pick(QuickPickKind::Case);
            return Ok(());
        }
        if builtin::SORT.matches(key) {
            self.open_quick_pick(QuickPickKind::Sort);
            return Ok(());
        }

        // QuickPick overlay handles only its own navigation; nothing else
        // falls through (so arrow keys don't also move the result list
        // behind the popup, and typing doesn't leak into the query bar).
        if let Overlay::QuickPick(kind) = self.overlay {
            self.handle_quick_pick_key(key, kind)?;
            return Ok(());
        }

        // Settings overlay handles its own navigation.
        if self.overlay == Overlay::Settings {
            self.handle_settings_key(key, terminal)?;
            return Ok(());
        }

        // Configured keybinds (result actions and query-editing alike). First
        // matching line wins; reserved built-ins were already handled above.
        for (key_spec, action) in self.settings.binds.clone() {
            if let Some(parsed) = KeyBind::parse(&key_spec) {
                if parsed.matches(key) {
                    self.dispatch_action(action, terminal)?;
                    return Ok(());
                }
            }
        }

        // List navigation.
        match key.code {
            KeyCode::Up => {
                self.move_selection(-1);
                return Ok(());
            }
            KeyCode::Down => {
                self.move_selection(1);
                return Ok(());
            }
            KeyCode::PageUp => {
                self.move_selection(-(self.list_visible_rows.max(1) as isize));
                return Ok(());
            }
            KeyCode::PageDown => {
                self.move_selection(self.list_visible_rows.max(1) as isize);
                return Ok(());
            }
            KeyCode::Home => {
                if !self.entries.is_empty() {
                    self.selection.select(Some(0));
                    self.queue_preview();
                }
                return Ok(());
            }
            KeyCode::End => {
                if !self.entries.is_empty() {
                    self.selection.select(Some(self.entries.len() - 1));
                    self.queue_preview();
                }
                return Ok(());
            }
            _ => {}
        }

        // Enter runs the search.
        if matches!(key.code, KeyCode::Enter) {
            self.run_search();
            return Ok(());
        }

        // Query input.
        match key.code {
            KeyCode::Char(ch) => {
                if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
                    // A Ctrl/Alt chord that reached here matched no configured
                    // bind. Swallow it so control chars never leak into the
                    // query. (Query editing — clear/delete-word/etc. — is a
                    // regular `bind.<key> = <action>` handled in the bind loop
                    // above; if the user didn't bind it, it simply does nothing.)
                    return Ok(());
                }
                self.insert_char(ch);
            }
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete_forward(),
            KeyCode::Left => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
            }
            KeyCode::Right => {
                if self.cursor < self.query.chars().count() {
                    self.cursor += 1;
                }
            }
            _ => {}
        }
        Ok(())
    }

    // ─── settings overlay (F2) ────────────────────────────────────────────
    // A draft editor for the persisted defaults. The three value rows are
    // edited in place with ←/→; Save persists them. Per the menu's framing
    // ("Default settings for new sessions") Save does NOT touch the live
    // session — that stays under F3/F4/F5 control until the next launch.

    fn open_settings(&mut self) {
        // The draft reflects the PERSISTED defaults, not the session's
        // F3/F4/F5 overrides — so read fresh from disk, falling back to the
        // in-memory settings if that fails.
        let persisted = load_settings().unwrap_or_else(|_| self.settings.clone());
        self.settings_draft = SettingsDraft::from_settings(&persisted);
        self.overlay = Overlay::Settings;
        self.settings_cursor = settings_row_index(SettingsRow::Mode);
    }

    fn settings_selectable(&self, row: SettingsRow) -> bool {
        match row {
            SettingsRow::Header | SettingsRow::Blank => false,
            // Save is only reachable/selectable once the draft diverges.
            SettingsRow::Save => self.settings_draft.dirty(),
            _ => true,
        }
    }

    fn handle_settings_key(&mut self, key: KeyEvent, terminal: &mut Tui) -> Result<()> {
        match key.code {
            KeyCode::Up => self.move_settings_cursor(-1),
            KeyCode::Down => self.move_settings_cursor(1),
            KeyCode::Left => self.cycle_settings_value(-1),
            KeyCode::Right => self.cycle_settings_value(1),
            KeyCode::Enter => self.activate_settings_row(terminal)?,
            _ => {}
        }
        Ok(())
    }

    /// Step the cursor to the next selectable row in `dir` (±1), skipping
    /// headers/blanks and the Save row when it's disabled. Stays put if there
    /// is no selectable row that way.
    fn move_settings_cursor(&mut self, dir: isize) {
        let n = SETTINGS_ROWS.len() as isize;
        let mut i = self.settings_cursor as isize;
        loop {
            i += dir;
            if i < 0 || i >= n {
                return;
            }
            if self.settings_selectable(SETTINGS_ROWS[i as usize]) {
                self.settings_cursor = i as usize;
                return;
            }
        }
    }

    /// ←/→ on a value row. Mode and case are binary (direction ignored); sort
    /// cycles through its four options with wraparound.
    fn cycle_settings_value(&mut self, dir: isize) {
        match SETTINGS_ROWS[self.settings_cursor] {
            SettingsRow::Mode => {
                self.settings_draft.mode = match self.settings_draft.mode {
                    SearchMode::Glob => SearchMode::Regex,
                    SearchMode::Regex => SearchMode::Glob,
                };
            }
            SettingsRow::Case => {
                self.settings_draft.case = !self.settings_draft.case;
            }
            SettingsRow::Sort => {
                const ORDER: [Sort; 4] = [
                    Sort::Depth,
                    Sort::Alphabetical,
                    Sort::BlameYoung,
                    Sort::BlameOld,
                ];
                let cur = ORDER
                    .iter()
                    .position(|s| *s == self.settings_draft.sort)
                    .unwrap_or(0) as isize;
                let len = ORDER.len() as isize;
                let next = ((cur + dir) % len + len) % len;
                self.settings_draft.sort = ORDER[next as usize];
            }
            _ => {}
        }
    }

    fn activate_settings_row(&mut self, terminal: &mut Tui) -> Result<()> {
        match SETTINGS_ROWS[self.settings_cursor] {
            SettingsRow::Save => {
                if self.settings_draft.dirty() {
                    self.save_default_settings()?;
                }
            }
            SettingsRow::EditConfig => self.edit_config_file(terminal)?,
            SettingsRow::ShowDefault => show_in_pager(DEFAULT_CONFIG_REFERENCE, terminal)?,
            _ => {}
        }
        Ok(())
    }

    fn save_default_settings(&mut self) -> Result<()> {
        let d = self.settings_draft;
        write_setting("search_mode", d.mode.token())?;
        write_setting("case_sensitive", if d.case { "true" } else { "false" })?;
        write_setting("sort", d.sort.token())?;
        // The on-disk baseline now matches the draft → clean, Save greys out.
        self.settings_draft.saved_mode = d.mode;
        self.settings_draft.saved_case = d.case;
        self.settings_draft.saved_sort = d.sort;
        // Defaults apply to NEW sessions only; the live session keeps whatever
        // F3/F4/F5 selected, so self.settings is intentionally untouched.
        self.flash_msg("defaults saved — applies to new sessions");
        // The cursor was on the (now-disabled) Save row; move it back up.
        self.settings_cursor = settings_row_index(SettingsRow::Sort);
        Ok(())
    }

    fn edit_config_file(&mut self, terminal: &mut Tui) -> Result<()> {
        let Some(path) = config_path() else {
            return Ok(());
        };
        if !path.exists() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(
                &path,
                "# yoink config — empty by default.\n\
                 # Press F2 → \"Show default config (reference)\" to see the full\n\
                 # annotated example, then copy the lines you want here.\n",
            );
        }
        let editor = editor_for_config();
        let path_clone = path.clone();
        suspend_tui_for(terminal, move || {
            // Same tty triplet trick as the editor open path so vim/nano see a
            // real terminal even though yoink's stdout is captured by the shell
            // wrapper.
            let tty_in = std::fs::OpenOptions::new().read(true).open("/dev/tty");
            let tty_out = std::fs::OpenOptions::new().write(true).open("/dev/tty");
            let tty_err = std::fs::OpenOptions::new().write(true).open("/dev/tty");
            let mut cmd = std::process::Command::new(&editor);
            cmd.arg(&path_clone);
            if let (Ok(i), Ok(o), Ok(e)) = (tty_in, tty_out, tty_err) {
                cmd.stdin(std::process::Stdio::from(i))
                    .stdout(std::process::Stdio::from(o))
                    .stderr(std::process::Stdio::from(e));
            }
            let _ = cmd.status();
        })?;
        // A manual edit is a direct action on the config, so reload it into the
        // live session and re-sync the draft baseline.
        self.settings = load_settings()?;
        self.settings_draft = SettingsDraft::from_settings(&self.settings);
        self.startup_warnings = detect_key_conflicts(&self.settings);
        if self.startup_warnings.is_empty() {
            self.flash_msg("config reloaded");
        } else {
            self.flash_msg(&format!(
                "config reloaded — ⚠ {} keybind conflict(s), see F1",
                self.startup_warnings.len()
            ));
        }
        if blame::blame_sort_active(&self.settings) {
            self.start_blame_warmup_if_needed();
        } else {
            self.run_search();
        }
        Ok(())
    }

    // ─── inline quick-pick menus (F3 / F4 / F5) ──────────────────
    // Each Alt-* opens a small popup listing all options. The cursor starts
    // on the current value so a glance + Enter is enough to confirm. The
    // selection is SESSION ONLY — never written to the config — so a
    // one-off blame view doesn't become the persistent default.

    fn open_quick_pick(&mut self, kind: QuickPickKind) {
        self.quick_pick_cursor = kind.current_index(&self.settings);
        self.overlay = Overlay::QuickPick(kind);
    }

    fn handle_quick_pick_key(&mut self, key: KeyEvent, kind: QuickPickKind) -> Result<()> {
        let len = kind.labels().len();
        match key.code {
            KeyCode::Up | KeyCode::Left => {
                if self.quick_pick_cursor > 0 {
                    self.quick_pick_cursor -= 1;
                }
            }
            KeyCode::Down | KeyCode::Right | KeyCode::Tab => {
                if self.quick_pick_cursor + 1 < len {
                    self.quick_pick_cursor += 1;
                }
            }
            KeyCode::Home => self.quick_pick_cursor = 0,
            KeyCode::End => self.quick_pick_cursor = len.saturating_sub(1),
            KeyCode::Enter => {
                self.apply_quick_pick(kind, self.quick_pick_cursor)?;
                self.overlay = Overlay::None;
            }
            _ => {}
        }
        Ok(())
    }

    fn apply_quick_pick(&mut self, kind: QuickPickKind, idx: usize) -> Result<()> {
        let prev_blame = blame::blame_sort_active(&self.settings);
        match kind {
            QuickPickKind::Mode => {
                self.settings.search_mode = match idx {
                    0 => SearchMode::Glob,
                    _ => SearchMode::Regex,
                };
                self.flash_msg(&format!(
                    "session search mode: {}  (not saved)",
                    self.settings.search_mode.token()
                ));
            }
            QuickPickKind::Case => {
                self.settings.case_sensitive = idx == 1;
                self.flash_msg(if self.settings.case_sensitive {
                    "session case: sensitive  (not saved)"
                } else {
                    "session case: insensitive  (not saved)"
                });
            }
            QuickPickKind::Sort => {
                self.settings.sort = match idx {
                    0 => Sort::Depth,
                    1 => Sort::Alphabetical,
                    2 => Sort::BlameYoung,
                    _ => Sort::BlameOld,
                };
                let label = match self.settings.sort {
                    Sort::Depth => "depth",
                    Sort::Alphabetical => "alphabetical",
                    Sort::BlameYoung => "youngest blame first",
                    Sort::BlameOld => "oldest blame first",
                };
                self.flash_msg(&format!("session sort: {label}  (not saved)"));
            }
        }
        // Only warm the blame cache when transitioning INTO a blame mode
        // from a non-blame mode. Picking alphabetical never touches the
        // cache, which is the whole reason for this menu.
        let now_blame = blame::blame_sort_active(&self.settings);
        if now_blame && !prev_blame {
            self.start_blame_warmup_if_needed();
        } else {
            self.run_search();
        }
        Ok(())
    }

    fn flash_msg(&mut self, msg: &str) {
        self.flash = Some((msg.to_string(), std::time::Instant::now()));
    }

    fn handle_mouse(&mut self, m: MouseEvent) {
        if self.overlay != Overlay::None {
            return;
        }
        match m.kind {
            MouseEventKind::ScrollUp => self.move_selection(-3),
            MouseEventKind::ScrollDown => self.move_selection(3),
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let max = self.entries.len() as isize - 1;
        let cur = self.selection.selected().unwrap_or(0) as isize;
        let mut next = cur + delta;
        if next < 0 {
            next = 0;
        }
        if next > max {
            next = max;
        }
        self.selection.select(Some(next as usize));
        self.queue_preview();
    }

    fn insert_char(&mut self, ch: char) {
        let mut chars: Vec<char> = self.query.chars().collect();
        if self.cursor > chars.len() {
            self.cursor = chars.len();
        }
        chars.insert(self.cursor, ch);
        self.query = chars.into_iter().collect();
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let mut chars: Vec<char> = self.query.chars().collect();
        chars.remove(self.cursor - 1);
        self.query = chars.into_iter().collect();
        self.cursor -= 1;
    }

    fn delete_forward(&mut self) {
        let mut chars: Vec<char> = self.query.chars().collect();
        if self.cursor < chars.len() {
            chars.remove(self.cursor);
            self.query = chars.into_iter().collect();
        }
    }

    fn clear_query(&mut self) {
        self.query.clear();
        self.cursor = 0;
    }

    fn delete_word(&mut self) {
        let chars: Vec<char> = self.query.chars().collect();
        if self.cursor == 0 {
            return;
        }
        let mut idx = self.cursor;
        while idx > 0 && chars[idx - 1].is_whitespace() {
            idx -= 1;
        }
        while idx > 0 && !chars[idx - 1].is_whitespace() {
            idx -= 1;
        }
        let new_query: String = chars[..idx].iter().chain(chars[self.cursor..].iter()).collect();
        self.query = new_query;
        self.cursor = idx;
    }

    fn run_search(&mut self) {
        let query = self.query.clone();
        let cwd = self.cwd.clone();
        let tx = self.events_tx.clone();
        // Snapshot the *session* settings (F3/F4/F5 toggles live here and are
        // never written to disk) so the worker honors them. Reloading from
        // disk inside the builders would silently ignore unsaved session
        // changes — the bug where switching glob→regex mid-session did nothing.
        let settings = self.settings.clone();
        let blame_active = blame::blame_sort_active(&self.settings);
        let _ = tx.send(AppEvent::Search(SearchProgress::Started));
        thread::spawn(move || {
            if query.trim().is_empty() {
                let _ = tx.send(AppEvent::Search(SearchProgress::Done(Vec::new())));
                return;
            }
            let result = if blame_active {
                build_blame_sorted_entries(&query, &cwd, &settings)
            } else {
                build_search_entries(&query, &cwd, &settings)
            };
            match result {
                Ok(entries) => {
                    let _ = tx.send(AppEvent::Search(SearchProgress::Done(entries)));
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::Search(SearchProgress::Failed(format!("{e}"))));
                }
            }
        });
    }

    fn queue_preview(&mut self) {
        let Some(idx) = self.selection.selected() else {
            return;
        };
        let Some(entry) = self.entries.get(idx) else {
            return;
        };
        let key = PreviewKey {
            path: entry.path.clone(),
            line: entry.line,
        };
        if self.preview.as_ref() == Some(&key) {
            return;
        }
        self.preview = Some(key.clone());
        self.preview_body = None;
        self.preview_loading = true;
        let cwd = self.cwd.clone();
        let tx = self.events_tx.clone();
        thread::spawn(move || match render_preview(&cwd, &key) {
            Ok(body) => {
                let _ = tx.send(AppEvent::Preview(PreviewProgress::Done { key, body }));
            }
            Err(e) => {
                let _ = tx.send(AppEvent::Preview(PreviewProgress::Failed {
                    key,
                    message: format!("preview failed: {e}"),
                }));
            }
        });
    }

    fn start_blame_warmup_if_needed(&mut self) {
        if !blame::blame_sort_active(&self.settings) || self.query.trim().is_empty() {
            self.run_search();
            return;
        }
        let query = self.query.clone();
        let cwd = self.cwd.clone();
        let tx = self.events_tx.clone();
        // Warm the cache for the file set the *session* settings select, so
        // the warmup matches the search the user will actually see.
        let settings = self.settings.clone();
        self.blame_progress = Some((0, 0, "preparing…".to_string()));
        thread::spawn(move || {
            // Do NOT clear the cache — `blame_times_cached` already
            // invalidates per-file on mtime change, so the warm cache from
            // a previous warmup is safely reusable across young ↔ old
            // toggles and across query changes that share files.
            let effective = match crate::search::effective_pattern(
                &query,
                settings.search_mode,
                settings.case_sensitive,
            ) {
                Ok(p) => p,
                Err(_) => {
                    let _ = tx.send(AppEvent::BlameDone);
                    return;
                }
            };
            let grouped =
                match crate::search::collect_rg_grouped_public(&effective, &cwd, &settings) {
                    Ok(g) => g,
                    Err(_) => {
                        let _ = tx.send(AppEvent::BlameDone);
                        return;
                    }
                };
            let total = grouped.len();
            for (i, (path, _)) in grouped.iter().enumerate() {
                let _ = tx.send(AppEvent::BlameProgress {
                    done: i + 1,
                    total,
                    current: path.to_string_lossy().to_string(),
                });
                let _ = crate::blame::blame_times_cached(&cwd, path);
            }
            let _ = tx.send(AppEvent::BlameDone);
        });
    }

    fn dispatch_action(&mut self, action: Action, terminal: &mut Tui) -> Result<()> {
        // Query-editing actions operate on the query box, not a result, so they
        // run before the "nothing selected" guard below.
        match action {
            Action::ClearQuery => {
                self.clear_query();
                return Ok(());
            }
            Action::DeleteWord => {
                self.delete_word();
                return Ok(());
            }
            Action::LineStart => {
                self.cursor = 0;
                return Ok(());
            }
            Action::LineEnd => {
                self.cursor = self.query.chars().count();
                return Ok(());
            }
            _ => {}
        }

        let Some(idx) = self.selection.selected() else {
            self.flash_msg("nothing selected");
            return Ok(());
        };
        let Some(entry) = self.entries.get(idx).cloned() else {
            return Ok(());
        };
        let rel = entry.path.to_string_lossy().to_string();
        match action {
            Action::Cd => {
                let target = actions::cd_target(&self.cwd, &rel);
                self.outcome = Some(Outcome::Cd(target));
                self.quit = true;
            }
            Action::Vim | Action::Vi | Action::Nano | Action::Cat => {
                let cmd = match action {
                    Action::Vim => "vim",
                    Action::Vi => "vi",
                    Action::Nano => "nano",
                    Action::Cat => "cat",
                    _ => unreachable!(),
                };
                let cwd = self.cwd.clone();
                let rel = rel.clone();
                suspend_tui_for(terminal, move || {
                    if let Err(e) = actions::open_in_editor(cmd, &cwd, &rel) {
                        eprintln!("yoink open error: {e}");
                        std::thread::sleep(Duration::from_millis(900));
                    }
                })?;
            }
            Action::VsCode | Action::Sublime | Action::Explorer => {
                let cmd = match action {
                    Action::VsCode => "code",
                    Action::Sublime => "subl",
                    Action::Explorer => "xdg-open",
                    _ => unreachable!(),
                };
                if let Err(e) = actions::open_detached(cmd, &self.cwd, &rel) {
                    self.flash_msg(&format!("{e}"));
                } else {
                    self.flash_msg(&format!("opened in {cmd}"));
                }
            }
            Action::CopyPath => {
                // Full absolute path on disk (with `:line` suffix for an
                // occurrence row). The relative path was confusing on
                // results in the cwd root — they looked identical to the
                // basename.
                let full = self.cwd.join(&entry.path);
                let mut text = full.display().to_string();
                if let Some(l) = entry.line {
                    text.push_str(&format!(":{l}"));
                }
                match actions::copy_to_clipboard(&text) {
                    Ok(()) => self.flash_msg(&format!("📋 path: {text}")),
                    Err(e) => self.flash_msg(&format!("clipboard error: {e}")),
                }
            }
            Action::CopyName => {
                let name = Path::new(&rel)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| rel.clone());
                match actions::copy_to_clipboard(&name) {
                    Ok(()) => self.flash_msg(&format!("📋 name: {name}")),
                    Err(e) => self.flash_msg(&format!("clipboard error: {e}")),
                }
            }
            // Query-editing actions are handled (and returned) above.
            Action::ClearQuery | Action::DeleteWord | Action::LineStart | Action::LineEnd => {
                unreachable!("query-edit actions are dispatched before the selection guard")
            }
        }
        let _ = self.pending_open.take();
        Ok(())
    }

    // ------------------- render -------------------

    fn draw(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // breadcrumb / title
                Constraint::Length(3), // query input
                Constraint::Min(8),    // body (results | preview)
                Constraint::Length(1), // hint line
                Constraint::Length(1), // status bar
            ])
            .split(area);

        self.draw_title(frame, chunks[0]);
        self.draw_query(frame, chunks[1]);
        self.draw_body(frame, chunks[2]);
        self.draw_hint(frame, chunks[3]);
        self.draw_status(frame, chunks[4]);

        match self.overlay {
            Overlay::Help => self.draw_help_overlay(frame, area),
            Overlay::Settings => self.draw_settings_overlay(frame, area),
            Overlay::QuickPick(kind) => self.draw_quick_pick_overlay(frame, area, kind),
            Overlay::Warning => self.draw_warning_overlay(frame, area),
            Overlay::None => {}
        }

        if let Some((flash, _)) = &self.flash {
            self.draw_flash(frame, area, flash);
        }
        if let Some((done, total, current)) = &self.blame_progress {
            self.draw_blame_progress(frame, area, *done, *total, current);
        }
    }

    fn draw_title(&self, frame: &mut Frame<'_>, area: Rect) {
        let cwd = self.cwd.display().to_string();
        let title = Line::from(vec![
            Span::styled("yoink", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(cwd, Style::default().fg(Color::DarkGray)),
        ]);
        frame.render_widget(Paragraph::new(title), area);
    }

    fn draw_query(&self, frame: &mut Frame<'_>, area: Rect) {
        let prompt = render_prompt_for(&self.settings);
        let prompt_span =
            Span::styled(prompt.clone(), Style::default().fg(Color::Green).add_modifier(Modifier::BOLD));
        let query_span = Span::raw(self.query.clone());
        let line = Line::from(vec![prompt_span, query_span]);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(if self.searching {
                Color::Yellow
            } else {
                Color::DarkGray
            }))
            .title(Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    if self.searching { "searching…" } else { "query" },
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(" "),
            ]));
        let para = Paragraph::new(line).block(block);
        frame.render_widget(para, area);
        // Cursor position inside the input: column = inside-block prompt + cursor
        let prompt_width = prompt.chars().count() as u16;
        let cursor_col = area.x + 1 + prompt_width + self.cursor as u16;
        let cursor_row = area.y + 1;
        if cursor_col < area.x + area.width.saturating_sub(1) {
            frame.set_cursor_position((cursor_col, cursor_row));
        }
    }

    fn draw_body(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(area);

        self.draw_results(frame, split[0]);
        self.draw_preview(frame, split[1]);
    }

    fn draw_results(&mut self, frame: &mut Frame<'_>, area: Rect) {
        self.list_visible_rows = area.height.saturating_sub(2) as usize;
        let items: Vec<ListItem> = self
            .entries
            .iter()
            .map(|entry| ListItem::new(ansi_to_text(&entry.display)))
            .collect();

        let title = if let Some(err) = &self.last_error {
            Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    format!("error: {err}"),
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
            ])
        } else {
            Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    format!("{} results", self.entries.len()),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(" "),
            ])
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(title);

        if self.entries.is_empty() && !self.searching {
            let msg = if self.query.trim().is_empty() {
                Text::from(vec![
                    Line::from(""),
                    Line::from(vec![Span::styled(
                        "  type a query and press Enter",
                        Style::default().fg(Color::DarkGray),
                    )]),
                    Line::from(""),
                    Line::from(vec![Span::styled(
                        "  F3: search mode   F4: case   F5: sort   F1: all binds",
                        Style::default().fg(Color::DarkGray),
                    )]),
                ])
            } else if self.last_error.is_some() {
                Text::from("")
            } else {
                Text::from(vec![
                    Line::from(""),
                    Line::from(vec![Span::styled(
                        "  no matches",
                        Style::default().fg(Color::DarkGray),
                    )]),
                ])
            };
            frame.render_widget(Paragraph::new(msg).block(block).wrap(Wrap { trim: false }), area);
            return;
        }

        let list = List::new(items).block(block).highlight_style(
            Style::default()
                .bg(Color::Rgb(50, 50, 70))
                .add_modifier(Modifier::BOLD),
        );

        frame.render_stateful_widget(list, area, &mut self.selection);
    }

    fn draw_preview(&self, frame: &mut Frame<'_>, area: Rect) {
        let title = match &self.preview {
            Some(k) => {
                let line = k.line.map(|l| format!(":{l}")).unwrap_or_default();
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled(
                        format!("{}{}", k.path.display(), line),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::raw(" "),
                ])
            }
            None => Line::from(" preview "),
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(title);

        let body = if self.preview.is_none() {
            Text::from(vec![Line::from(""),
                Line::from(vec![Span::styled(
                    "  no selection",
                    Style::default().fg(Color::DarkGray),
                )])])
        } else if self.preview_loading {
            Text::from(vec![Line::from(""),
                Line::from(vec![Span::styled(
                    "  loading…",
                    Style::default().fg(Color::DarkGray),
                )])])
        } else {
            self.preview_body.clone().unwrap_or_default()
        };

        frame.render_widget(Paragraph::new(body).block(block).wrap(Wrap { trim: false }), area);
    }

    fn draw_hint(&self, frame: &mut Frame<'_>, area: Rect) {
        let mut bits: Vec<Span> = Vec::new();
        bits.push(Span::styled(" Enter ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)));
        bits.push(Span::styled("search", Style::default().fg(Color::DarkGray)));
        bits.push(Span::raw("  "));
        bits.push(Span::styled("↑↓ ", Style::default().fg(Color::DarkGray)));
        bits.push(Span::styled("move", Style::default().fg(Color::DarkGray)));
        bits.push(Span::raw("  "));
        bits.push(Span::styled(
            format!(
                "{}/{}/{} ",
                builtin::MODE.label,
                builtin::CASE.label,
                builtin::SORT.label
            ),
            Style::default().fg(Color::Magenta),
        ));
        bits.push(Span::styled("mode/case/sort menu", Style::default().fg(Color::DarkGray)));
        bits.push(Span::raw("  "));
        bits.push(Span::styled(format!("{} ", builtin::SETTINGS.label), Style::default().fg(Color::Yellow)));
        bits.push(Span::styled("settings", Style::default().fg(Color::DarkGray)));
        bits.push(Span::raw("  "));
        bits.push(Span::styled(format!("{} ", builtin::HELP.label), Style::default().fg(Color::Yellow)));
        bits.push(Span::styled("all binds", Style::default().fg(Color::DarkGray)));
        bits.push(Span::raw("  "));
        // First two configured open-action binds as inline hints.
        for (k, a) in self.settings.binds.iter().take(3) {
            bits.push(Span::styled(format!("{k} ", k = k), Style::default().fg(Color::Cyan)));
            bits.push(Span::styled(a.label().to_string(), Style::default().fg(Color::DarkGray)));
            bits.push(Span::raw("  "));
        }
        frame.render_widget(Paragraph::new(Line::from(bits)), area);
    }

    fn draw_status(&self, frame: &mut Frame<'_>, area: Rect) {
        let mode_label = match self.settings.search_mode {
            SearchMode::Glob => " glob ",
            SearchMode::Regex => " regex ",
        };
        let case_label = if self.settings.case_sensitive {
            " Aa "
        } else {
            " aA "
        };
        let case_tip = if self.settings.case_sensitive {
            "sensitive"
        } else {
            "insensitive"
        };
        let sort_label = match self.settings.sort {
            Sort::Depth => " sort: depth ",
            Sort::Alphabetical => " sort: alphabetical ",
            Sort::BlameYoung => " sort: youngest blame ",
            Sort::BlameOld => " sort: oldest blame ",
        };

        let chip = |s: &str, bg: Color, fg: Color| {
            Span::styled(
                s.to_string(),
                Style::default().bg(bg).fg(fg).add_modifier(Modifier::BOLD),
            )
        };

        let mut line = vec![
            chip(mode_label, Color::Rgb(70, 100, 200), Color::White),
            Span::raw(" "),
            chip(case_label, Color::Rgb(100, 70, 150), Color::White),
            Span::raw(" "),
            Span::styled(case_tip, Style::default().fg(Color::DarkGray)),
            Span::raw("   "),
            chip(sort_label, Color::Rgb(170, 80, 50), Color::White),
        ];
        if self.settings.search_mode == SearchMode::Regex && !self.settings.case_sensitive {
            line.push(Span::raw("   "));
            line.push(Span::styled(
                "regex prefix: (?i)",
                Style::default().fg(Color::Yellow),
            ));
        }
        if self.searching {
            line.push(Span::raw("   "));
            line.push(Span::styled(
                "● searching",
                Style::default().fg(Color::Yellow),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(line)), area);
    }

    fn draw_warning_overlay(&self, frame: &mut Frame<'_>, area: Rect) {
        let popup = centered_rect(70, 50, area);
        frame.render_widget(Clear, popup);

        let mut lines: Vec<Line> = vec![
            Line::from(""),
            Line::from(vec![Span::styled(
                format!(
                    "  {} keybind conflict(s) in your config:",
                    self.startup_warnings.len()
                ),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            )]),
            Line::from(""),
        ];
        for w in &self.startup_warnings {
            lines.push(Line::from(vec![
                Span::styled("  • ", Style::default().fg(Color::Yellow)),
                Span::styled(w.clone(), Style::default().fg(Color::White)),
            ]));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "  When a key has more than one meaning, a reserved built-in wins;",
            Style::default().fg(Color::DarkGray),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  otherwise the first matching bind line wins and the rest do",
            Style::default().fg(Color::DarkGray),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  nothing. Edit ~/.yoink-config (F2 → Edit configuration file).",
            Style::default().fg(Color::DarkGray),
        )]));
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "  press any key to dismiss",
            Style::default().fg(Color::DarkGray),
        )]));

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Double)
            .border_style(Style::default().fg(Color::Yellow))
            .title(Line::from(" ⚠ keybind conflicts "));
        frame.render_widget(
            Paragraph::new(lines).block(block).wrap(Wrap { trim: false }),
            popup,
        );
    }

    fn draw_help_overlay(&self, frame: &mut Frame<'_>, area: Rect) {
        let popup = centered_rect(70, 80, area);
        frame.render_widget(Clear, popup);

        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(vec![Span::styled(
            "yoink — key bindings",
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )]));
        lines.push(Line::from(""));
        lines.push(help_row("Enter", "run the search"));
        lines.push(help_row("↑ / ↓ / PgUp / PgDn", "move selection"));
        lines.push(help_row("Esc", "close overlay / clear focus"));
        lines.push(help_row(builtin::QUIT.label, builtin::QUIT.desc));
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "session pickers (open a menu, ↑↓ + Enter — never saved):",
            Style::default().fg(Color::DarkGray),
        )]));
        lines.push(help_row(builtin::MODE.label, builtin::MODE.desc));
        lines.push(help_row(builtin::CASE.label, builtin::CASE.desc));
        lines.push(help_row(builtin::SORT.label, builtin::SORT.desc));
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "to change the default for new sessions: F2 → edit the values → Save",
            Style::default().fg(Color::DarkGray),
        )]));
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "overlays:",
            Style::default().fg(Color::DarkGray),
        )]));
        lines.push(help_row(builtin::HELP.label, builtin::HELP.desc));
        lines.push(help_row(builtin::SETTINGS.label, builtin::SETTINGS.desc));
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "configured keybinds (bind.<key> in ~/.yoink-config):",
            Style::default().fg(Color::DarkGray),
        )]));
        if self.settings.binds.is_empty() {
            lines.push(Line::from(vec![Span::styled(
                "  (none configured)",
                Style::default().fg(Color::DarkGray),
            )]));
        } else {
            for (key, action) in &self.settings.binds {
                lines.push(help_row(key, action.label()));
            }
        }
        if !self.startup_warnings.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                "⚠ keybind conflicts (built-in or first bind wins; rest do nothing):",
                Style::default().fg(Color::Yellow),
            )]));
            for w in &self.startup_warnings {
                lines.push(Line::from(vec![
                    Span::styled("  • ", Style::default().fg(Color::Yellow)),
                    Span::styled(w.clone(), Style::default().fg(Color::White).dim()),
                ]));
            }
        }
        lines.push(Line::from(""));
        if let Some(path) = &self.settings.source_path {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled("config: ", Style::default().fg(Color::DarkGray)),
                Span::styled(path.display().to_string(), Style::default().fg(Color::Cyan)),
            ]));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "press Esc or F1 to close",
            Style::default().fg(Color::DarkGray),
        )]));

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Double)
            .border_style(Style::default().fg(Color::Yellow))
            .title(Line::from(" help "));
        frame.render_widget(
            Paragraph::new(lines).block(block).wrap(Wrap { trim: false }),
            popup,
        );
    }

    fn draw_quick_pick_overlay(&self, frame: &mut Frame<'_>, area: Rect, kind: QuickPickKind) {
        let labels = kind.labels();
        // Width tracks the widest option label + arrow + padding; clamped to
        // the terminal width.
        let max_label = labels.iter().map(|s| s.chars().count()).max().unwrap_or(20);
        let title_len = kind.title().chars().count();
        let footer_len = "↑↓ choose · Enter confirm · Esc cancel".chars().count();
        let inner_w = max_label.max(title_len).max(footer_len) + 6;
        let width = (inner_w as u16 + 4).min(area.width.saturating_sub(4));
        // Height: title + blank + options + blank + footer + borders.
        let height = (labels.len() as u16) + 6;

        let x = area.x + area.width.saturating_sub(width) / 2;
        let y = area.y + area.height.saturating_sub(height) / 2;
        let popup = Rect { x, y, width, height };
        frame.render_widget(Clear, popup);

        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(""));
        for (i, label) in labels.iter().enumerate() {
            let is_cursor = i == self.quick_pick_cursor;
            let marker = if is_cursor { " ▶ " } else { "   " };
            let style = if is_cursor {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            lines.push(Line::from(vec![
                Span::styled(marker, Style::default().fg(Color::Yellow)),
                Span::styled((*label).to_string(), style),
            ]));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                "↑↓ choose · Enter confirm · Esc cancel",
                Style::default().fg(Color::DarkGray),
            ),
        ]));

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Double)
            .border_style(Style::default().fg(Color::Magenta))
            .title(Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    kind.title(),
                    Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
            ]));
        frame.render_widget(
            Paragraph::new(lines).block(block).wrap(Wrap { trim: false }),
            popup,
        );
    }

    fn draw_settings_overlay(&self, frame: &mut Frame<'_>, area: Rect) {
        let popup = centered_rect(60, 60, area);
        frame.render_widget(Clear, popup);

        let draft = &self.settings_draft;
        let dirty = draft.dirty();
        let mode_label = match draft.mode {
            SearchMode::Glob => "glob",
            SearchMode::Regex => "regex",
        };
        let case_label = if draft.case { "sensitive" } else { "insensitive" };
        let sort_label = match draft.sort {
            Sort::Depth => "depth",
            Sort::Alphabetical => "alphabetical",
            Sort::BlameYoung => "youngest blame first",
            Sort::BlameOld => "oldest blame first",
        };

        // A value row: indented label + a cyan `‹ value ›` showing it cycles.
        let value_row = |label: &str, value: &str| {
            Line::from(vec![
                Span::raw("    "),
                Span::styled(format!("{label:<14}"), Style::default().fg(Color::White)),
                Span::styled("‹ ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    value.to_string(),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" ›", Style::default().fg(Color::DarkGray)),
            ])
        };

        let mut items: Vec<ListItem> = Vec::with_capacity(SETTINGS_ROWS.len());
        for row in SETTINGS_ROWS {
            let line = match row {
                SettingsRow::Header => Line::from(vec![Span::styled(
                    "Default settings for new sessions:",
                    Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
                )]),
                SettingsRow::Mode => value_row("search mode", mode_label),
                SettingsRow::Case => value_row("sensitivity", case_label),
                SettingsRow::Sort => value_row("sorting", sort_label),
                SettingsRow::Save => {
                    if dirty {
                        Line::from(vec![
                            Span::raw("    "),
                            Span::styled(
                                "Save",
                                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(
                                "   (write defaults to config)",
                                Style::default().fg(Color::DarkGray),
                            ),
                        ])
                    } else {
                        // Greyed + non-selectable until something changes.
                        Line::from(vec![
                            Span::raw("    "),
                            Span::styled("Save", Style::default().fg(Color::DarkGray)),
                            Span::styled(
                                "   (no changes)",
                                Style::default().fg(Color::DarkGray),
                            ),
                        ])
                    }
                }
                SettingsRow::Blank => Line::from(""),
                SettingsRow::EditConfig => Line::from(vec![
                    Span::raw("  "),
                    Span::styled("Edit configuration file", Style::default().fg(Color::White)),
                ]),
                SettingsRow::ShowDefault => Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        "Show default configuration (reference)",
                        Style::default().fg(Color::White),
                    ),
                ]),
            };
            items.push(ListItem::new(line));
        }

        let mut state = ListState::default();
        state.select(Some(self.settings_cursor.min(SETTINGS_ROWS.len().saturating_sub(1))));
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Double)
            .border_style(Style::default().fg(Color::Magenta))
            .title(Line::from(" settings "))
            .title_bottom(Line::from(
                " ↑↓ move · ←→ change · Enter select · Esc close ",
            ));
        let list = List::new(items)
            .block(block)
            .highlight_style(
                Style::default()
                    .bg(Color::Rgb(60, 60, 90))
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ ");
        frame.render_stateful_widget(list, popup, &mut state);
    }

    fn draw_flash(&self, frame: &mut Frame<'_>, area: Rect, msg: &str) {
        let width = msg.chars().count() as u16 + 4;
        let popup = Rect {
            x: area.x + area.width.saturating_sub(width + 2),
            y: area.y + 1,
            width: width.min(area.width.saturating_sub(2)),
            height: 1,
        };
        frame.render_widget(Clear, popup);
        let line = Line::from(vec![
            Span::raw(" "),
            Span::styled(msg.to_string(), Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw(" "),
        ]);
        frame.render_widget(Paragraph::new(line), popup);
    }

    fn draw_blame_progress(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        done: usize,
        total: usize,
        current: &str,
    ) {
        let popup = centered_rect(70, 20, area);
        frame.render_widget(Clear, popup);
        let bar_width = popup.width.saturating_sub(8) as usize;
        let pct = if total == 0 { 0 } else { (done * 100) / total };
        let filled = if total == 0 { 0 } else { (done * bar_width) / total };
        let mut bar = String::with_capacity(bar_width);
        for i in 0..bar_width {
            bar.push(if i < filled { '█' } else { '░' });
        }
        let lines = vec![
            Line::from(""),
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "⏳ pre-warming git blame cache…",
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::raw("  "),
                Span::styled(bar, Style::default().fg(Color::Cyan)),
                Span::raw(format!("  {pct:>3}%   {done}/{total}")),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    truncate(current, popup.width as usize - 4),
                    Style::default().fg(Color::DarkGray),
                ),
            ]),
        ];
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Double)
            .border_style(Style::default().fg(Color::Yellow));
        frame.render_widget(
            Paragraph::new(lines).block(block).wrap(Wrap { trim: false }),
            popup,
        );
    }
}

fn help_row(key: &str, desc: &str) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("{key:<22}"),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(desc.to_string(), Style::default().fg(Color::White).dim()),
    ])
}

/// Annotated default config, baked into the binary. Users can browse this
/// via F2 → "Show default config (reference)" to learn the syntax and copy
/// snippets into their own `~/.yoink-config` (which is empty by default).
pub const DEFAULT_CONFIG_REFERENCE: &str = include_str!("../.yoink-config");

/// Rows of the F2 settings overlay, in display order. Headers and the blank
/// separator are non-selectable; navigation skips them (see
/// `App::settings_selectable`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsRow {
    Header,
    Mode,
    Case,
    Sort,
    Save,
    Blank,
    EditConfig,
    ShowDefault,
}

const SETTINGS_ROWS: [SettingsRow; 8] = [
    SettingsRow::Header,
    SettingsRow::Mode,
    SettingsRow::Case,
    SettingsRow::Sort,
    SettingsRow::Save,
    SettingsRow::Blank,
    SettingsRow::EditConfig,
    SettingsRow::ShowDefault,
];

fn settings_row_index(row: SettingsRow) -> usize {
    SETTINGS_ROWS.iter().position(|r| *r == row).unwrap_or(0)
}

/// Reserved, non-rebindable keys, paired with what they do. Used only to flag
/// config binds that would be shadowed by a built-in.
fn reserved_keys() -> Vec<(KeyBind, &'static str)> {
    use crate::keys::builtin::*;
    let mut v: Vec<(KeyBind, &'static str)> = [HELP, SETTINGS, MODE, CASE, SORT, QUIT]
        .iter()
        .map(|b| (b.bind(), b.desc))
        .collect();
    // Navigation / search keys are reserved too, though rarely bound.
    for (spec, desc) in [
        ("enter", "run search"),
        ("esc", "close overlay"),
        ("up", "move up"),
        ("down", "move down"),
        ("pageup", "page up"),
        ("pagedown", "page down"),
        ("home", "first result"),
        ("end", "last result"),
    ] {
        if let Some(k) = KeyBind::parse(spec) {
            v.push((k, desc));
        }
    }
    v
}

/// Find keys mapped to more than one thing across the reserved built-ins and
/// the configured binds (result *and* query-editing alike now share the
/// `bind.<key>` namespace). Returns one human-readable line per conflicting
/// key, sorted for stable output. Handles any number of conflicts, including a
/// single key claimed by 3+ targets and the same key bound twice in the config.
fn detect_key_conflicts(settings: &YoinkSettings) -> Vec<String> {
    let mut entries: Vec<(KeyBind, String)> = reserved_keys()
        .into_iter()
        .map(|(bind, desc)| (bind, format!("reserved: {desc}")))
        .collect();

    for (spec, action) in &settings.binds {
        if let Some(bind) = KeyBind::parse(spec) {
            entries.push((bind, action.label().to_string()));
        }
    }

    conflicts_among(entries)
}

/// Group `(key, target)` pairs by key and report every key claimed by more
/// than one target (3-way and beyond included). One line per conflicting key,
/// sorted for stable output.
fn conflicts_among(entries: Vec<(KeyBind, String)>) -> Vec<String> {
    use std::collections::HashMap;
    let mut map: HashMap<KeyBind, Vec<String>> = HashMap::new();
    for (bind, label) in entries {
        map.entry(bind).or_default().push(label);
    }
    let mut out: Vec<String> = map
        .into_iter()
        .filter(|(_, targets)| targets.len() > 1)
        .map(|(bind, targets)| format!("{} → {}", bind.display(), targets.join("  vs  ")))
        .collect();
    out.sort();
    out
}

/// Show a long body of text in `less` (or print it if less is missing).
/// Suspends the TUI for the duration. Like the editor path, hands the
/// pager its own `/dev/tty` so it sees a real terminal.
fn show_in_pager(body: &str, terminal: &mut Tui) -> Result<()> {
    let body = body.to_string();
    suspend_tui_for(terminal, move || {
        if which::which("less").is_ok() {
            let tty_out = std::fs::OpenOptions::new().write(true).open("/dev/tty");
            let tty_err = std::fs::OpenOptions::new().write(true).open("/dev/tty");
            let mut cmd = std::process::Command::new("less");
            cmd.arg("-R").arg("-+F");
            if let (Ok(o), Ok(e)) = (tty_out, tty_err) {
                cmd.stdout(std::process::Stdio::from(o))
                    .stderr(std::process::Stdio::from(e));
            }
            let child = cmd.stdin(std::process::Stdio::piped()).spawn();
            if let Ok(mut child) = child {
                if let Some(stdin) = child.stdin.as_mut() {
                    use std::io::Write;
                    let _ = stdin.write_all(body.as_bytes());
                }
                let _ = child.wait();
            }
        } else {
            // Fallback: dump to /dev/tty so it doesn't pollute yoink's stdout.
            if let Ok(mut tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty") {
                use std::io::Write;
                let _ = tty.write_all(body.as_bytes());
                let _ = tty.write_all(b"\n[press Enter to return]\n");
                let mut buf = String::new();
                let _ = std::io::stdin().read_line(&mut buf);
            }
        }
    })?;
    Ok(())
}


fn render_prompt_for(settings: &YoinkSettings) -> String {
    let sort = match settings.sort {
        Sort::Depth => "",
        Sort::Alphabetical => " · alpha",
        Sort::BlameYoung => " · young",
        Sort::BlameOld => " · old",
    };
    match (settings.search_mode, settings.case_sensitive) {
        (SearchMode::Regex, false) => format!("regex{sort}> (?i)"),
        (SearchMode::Regex, true) => format!("regex{sort}> "),
        (SearchMode::Glob, false) => format!("glob/i{sort}> "),
        (SearchMode::Glob, true) => format!("glob{sort}> "),
    }
}

fn editor_for_config() -> String {
    if let Some(v) = std::env::var_os("EDITOR") {
        let s = v.to_string_lossy().to_string();
        if !s.is_empty() {
            return s;
        }
    }
    if which::which("nano").is_ok() {
        return "nano".to_string();
    }
    if which::which("vim").is_ok() {
        return "vim".to_string();
    }
    "vi".to_string()
}

fn ansi_to_text(s: &str) -> Text<'static> {
    match s.into_text() {
        Ok(t) => t,
        Err(_) => Text::from(strip_ansi(s)),
    }
}

fn strip_ansi(s: &str) -> String {
    // Trivial fallback if ansi-to-tui can't parse — drop CSI sequences.
    let mut out = String::with_capacity(s.len());
    let mut iter = s.chars().peekable();
    while let Some(c) = iter.next() {
        if c == '\x1b' && iter.peek() == Some(&'[') {
            iter.next();
            while let Some(&n) = iter.peek() {
                iter.next();
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Run bat to produce a syntax-highlighted preview body. Returns the raw
/// ANSI text; the renderer converts it to ratatui `Text` on the UI thread.
/// Includes a one-line blame header above the file body.
fn render_preview(cwd: &Path, key: &PreviewKey) -> Result<String> {
    use std::process::Command;
    let full = cwd.join(&key.path);

    let mut header = String::new();
    header.push_str(&blame_header(cwd, &key.path, key.line));
    header.push('\n');
    header.push_str(&"─".repeat(40));
    header.push('\n');

    if full.is_dir() {
        let listing = Command::new("ls")
            .arg("-la")
            .arg("--color=always")
            .arg(&full)
            .output()
            .context("ls failed")?;
        let mut body = header;
        body.push_str(&String::from_utf8_lossy(&listing.stdout));
        return Ok(body);
    }

    let mut bat = Command::new("bat");
    bat.arg("--style=numbers").arg("--color=always").arg("--paging=never");
    if let Some(line) = key.line {
        let ctx = 30usize;
        let start = if line > ctx { line - ctx } else { 1 };
        let end = line + ctx;
        bat.arg("--highlight-line")
            .arg(line.to_string())
            .arg("--line-range")
            .arg(format!("{start}:{end}"));
    } else {
        bat.arg("--line-range=:300");
    }
    let output = bat.arg(&full).output().context("bat failed")?;
    let mut body = header;
    if output.status.success() {
        body.push_str(&String::from_utf8_lossy(&output.stdout));
    } else {
        body.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    Ok(body)
}

fn blame_header(cwd: &Path, rel: &Path, line: Option<usize>) -> String {
    use crate::blame::{
        blame_for_line_cached, file_last_touched, find_repo_root, format_unix_date,
        latest_change_from_map, line_summary_from_map, try_blame_from_cache,
    };
    let abs = cwd.join(rel);
    if abs.parent().and_then(find_repo_root).is_none() {
        return "\x1b[2;37m📜 git blame: file is not inside a git working tree\x1b[0m".to_string();
    }
    if let Some(map) = try_blame_from_cache(cwd, rel) {
        if let Some(line) = line {
            if let Some(summary) = line_summary_from_map(&map, line) {
                return format!(
                    "\x1b[1;33m📜 git blame\x1b[0m  \x1b[1;36mL{line}\x1b[0m  \x1b[37m{summary}\x1b[0m"
                );
            }
        }
        if let Some((ts, author)) = latest_change_from_map(&map) {
            return format!(
                "\x1b[1;33m📜 git blame\x1b[0m  \x1b[37mlast touched\x1b[0m \x1b[1;35m{}\x1b[0m \x1b[37m{author}\x1b[0m",
                format_unix_date(ts)
            );
        }
    }
    if let Some(line) = line {
        if let Some(info) = blame_for_line_cached(cwd, rel, line) {
            let sha: String = info.sha.chars().take(8).collect();
            return format!(
                "\x1b[1;33m📜 git blame\x1b[0m  \x1b[1;36mL{line}\x1b[0m  \x1b[37m{sha} {} {}\x1b[0m",
                format_unix_date(info.timestamp),
                info.author
            );
        }
    }
    if let Some((ts, author)) = file_last_touched(cwd, rel) {
        return format!(
            "\x1b[1;33m📜 git blame\x1b[0m  \x1b[37mlast touched\x1b[0m \x1b[1;35m{}\x1b[0m \x1b[37m{author}\x1b[0m",
            format_unix_date(ts)
        );
    }
    "\x1b[2;37m📜 git blame: file is untracked or has no history\x1b[0m".to_string()
}

// Silence unused-warning for the Write import which is only used in a small
// number of places that may be compiled out under future changes.
#[allow(dead_code)]
fn _force_write_used(_w: &mut dyn Write) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn kb(spec: &str) -> KeyBind {
        KeyBind::parse(spec).unwrap()
    }

    #[test]
    fn no_conflicts_when_keys_are_distinct() {
        let entries = vec![
            (kb("ctrl-u"), "clear query".to_string()),
            (kb("ctrl-w"), "delete word".to_string()),
            (kb("ctrl-o"), "vscode".to_string()),
        ];
        assert!(conflicts_among(entries).is_empty());
    }

    #[test]
    fn reports_multiple_conflicts_including_a_three_way() {
        let entries = vec![
            // Two-way: ctrl-e claimed by cd and jump-to-end.
            (kb("ctrl-e"), "cd".to_string()),
            (kb("ctrl-e"), "jump to end of query".to_string()),
            // Three-way on ctrl-a.
            (kb("ctrl-a"), "jump to start of query".to_string()),
            (kb("ctrl-a"), "vim".to_string()),
            (kb("ctrl-a"), "reserved: something".to_string()),
            // Unique — must not be reported.
            (kb("ctrl-w"), "delete word".to_string()),
        ];
        let conflicts = conflicts_among(entries);
        assert_eq!(conflicts.len(), 2, "expected exactly two conflicting keys");
        // Sorted output: "Ctrl-A …" before "Ctrl-E …".
        assert!(conflicts[0].starts_with("Ctrl-A → "));
        assert!(conflicts[0].contains("jump to start of query"));
        assert!(conflicts[0].contains("vim"));
        assert!(conflicts[0].contains("reserved: something"));
        assert!(conflicts[1].starts_with("Ctrl-E → "));
        assert!(conflicts[1].contains("cd"));
        assert!(conflicts[1].contains("jump to end of query"));
    }

    #[test]
    fn case_insensitive_keys_collide() {
        // `KeyBind` treats ctrl-A / ctrl-a as the same key, so they conflict.
        let entries = vec![
            (kb("ctrl-A"), "one".to_string()),
            (kb("ctrl-a"), "two".to_string()),
        ];
        assert_eq!(conflicts_among(entries).len(), 1);
    }
}
