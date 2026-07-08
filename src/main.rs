use std::{
    cmp::{max, min},
    collections::{HashMap, HashSet},
    fs,
    io::{self, Stdout},
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use directories::ProjectDirs;
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};
use serde::{Deserialize, Serialize};

#[derive(Parser, Debug)]
#[command(
    name = "gamevim",
    about = "Patch Courier: a Vim-learning terminal game"
)]
struct Cli {
    #[arg(long)]
    list_levels: bool,
    #[arg(long)]
    lesson: Option<String>,
    #[arg(long)]
    seed: Option<u64>,
    #[arg(long)]
    reset_progress: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let catalog = Catalog::new();
    let mut progress = ProgressStore::load()?;

    if cli.reset_progress {
        ProgressStore::reset()?;
        println!("Progress reset.");
        return Ok(());
    }

    if cli.list_levels {
        for lesson in &catalog.lessons {
            println!("Lesson {}: {}", lesson.number, lesson.title);
            println!("  vocabulary: {}", lesson.commands.help());
            for level in &lesson.levels {
                let status = if progress.is_unlocked(&level.id) {
                    if progress.levels.get(&level.id).is_some_and(|p| p.cleared) {
                        "cleared"
                    } else {
                        "unlocked"
                    }
                } else {
                    "locked"
                };
                println!("  {:<3} {:<28} {}", level.id, level.title, status);
            }
        }
        return Ok(());
    }

    if let Some(level_id) = cli.lesson {
        let seed = cli.seed.unwrap_or(DEFAULT_SEED);
        let outcome = run_single_level_cli(&catalog, &mut progress, &level_id, seed)?;
        println!("{}", outcome.summary());
        progress.save()?;
        return Ok(());
    }

    run_tui(catalog, progress, cli.seed.unwrap_or(DEFAULT_SEED))
}

const DEFAULT_SEED: u64 = 7;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Progress {
    levels: HashMap<String, LevelProgress>,
    last_played: Option<String>,
    color_intense: bool,
    menu_arrows: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct LevelProgress {
    cleared: bool,
    best_time_ms: Option<u128>,
    best_commands: Option<usize>,
}

impl Progress {
    fn new() -> Self {
        Self {
            color_intense: true,
            menu_arrows: true,
            ..Self::default()
        }
    }

    fn is_unlocked(&self, level_id: &str) -> bool {
        if level_id.starts_with("1-") {
            return true;
        }
        if level_id.starts_with("2-") {
            return self.is_cleared("1-2");
        }
        if level_id.starts_with("3-") {
            return self.is_cleared("2-2");
        }
        false
    }

    fn is_cleared(&self, level_id: &str) -> bool {
        self.levels.get(level_id).is_some_and(|p| p.cleared)
    }

    fn record_clear(&mut self, level_id: &str, elapsed: Duration, commands: usize) {
        let entry = self.levels.entry(level_id.to_string()).or_default();
        entry.cleared = true;
        let elapsed_ms = elapsed.as_millis();
        entry.best_time_ms = Some(
            entry
                .best_time_ms
                .map_or(elapsed_ms, |best| best.min(elapsed_ms)),
        );
        entry.best_commands = Some(
            entry
                .best_commands
                .map_or(commands, |best| best.min(commands)),
        );
        self.last_played = Some(level_id.to_string());
    }
}

struct ProgressStore;

impl ProgressStore {
    fn load() -> Result<Progress> {
        let path = progress_path()?;
        if !path.exists() {
            return Ok(Progress::new());
        }
        let data = fs::read_to_string(&path)
            .with_context(|| format!("reading progress file {}", path.display()))?;
        serde_json::from_str(&data).context("parsing progress file")
    }

    fn reset() -> Result<()> {
        let path = progress_path()?;
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

impl Progress {
    fn save(&self) -> Result<()> {
        let path = progress_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

fn progress_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("dev", "Gamevim", "Gamevim")
        .context("could not locate user data directory")?;
    Ok(dirs.data_dir().join("progress.json"))
}

#[derive(Debug, Clone)]
struct Catalog {
    lessons: Vec<Lesson>,
}

impl Catalog {
    fn new() -> Self {
        Self {
            lessons: vec![lesson_one(), lesson_two(), lesson_three()],
        }
    }

    fn all_levels(&self) -> Vec<&Level> {
        self.lessons
            .iter()
            .flat_map(|lesson| lesson.levels.iter())
            .collect()
    }

    fn level(&self, id: &str) -> Option<&Level> {
        self.lessons
            .iter()
            .flat_map(|lesson| lesson.levels.iter())
            .find(|level| level.id == id)
    }

    fn next_recommended<'a>(&'a self, progress: &Progress) -> Option<&'a Level> {
        self.all_levels()
            .into_iter()
            .find(|level| progress.is_unlocked(&level.id) && !progress.is_cleared(&level.id))
            .or_else(|| {
                self.all_levels()
                    .into_iter()
                    .find(|level| progress.is_unlocked(&level.id))
            })
    }
}

#[derive(Debug, Clone)]
struct Lesson {
    number: u8,
    title: &'static str,
    commands: CommandSet,
    levels: Vec<Level>,
}

#[derive(Debug, Clone)]
struct Level {
    id: String,
    title: &'static str,
    lesson: u8,
    kind: LevelKind,
    required_to_unlock_next: bool,
}

#[derive(Debug, Clone)]
enum LevelKind {
    Grid(GridSpec),
    Token(TokenSpec),
    Edit(EditSpec),
}

#[derive(Debug, Clone)]
struct GridSpec {
    width: usize,
    height: usize,
    start: Pos,
    goal: Pos,
    command_budget: usize,
    lava: bool,
    lava_interval_ms: Option<u64>,
    generated: bool,
    mastery: bool,
}

#[derive(Debug, Clone)]
struct TokenSpec {
    lines: Vec<&'static str>,
    targets: Vec<(usize, usize)>,
    command_budget: usize,
    timer_seconds: Option<u64>,
    generated: bool,
}

#[derive(Debug, Clone)]
struct EditSpec {
    start: Vec<&'static str>,
    expected: Vec<&'static str>,
    cursor: Pos,
    command_budget: usize,
    timer_seconds: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Pos {
    x: usize,
    y: usize,
}

fn lesson_one() -> Lesson {
    Lesson {
        number: 1,
        title: "Lava Escape: Cursor Motions",
        commands: CommandSet::for_lesson(1),
        levels: vec![
            Level {
                id: "1-0".to_string(),
                title: "Tutorial: The Warm Floor",
                lesson: 1,
                required_to_unlock_next: false,
                kind: LevelKind::Grid(GridSpec {
                    width: 25,
                    height: 11,
                    start: Pos { x: 2, y: 9 },
                    goal: Pos { x: 22, y: 1 },
                    command_budget: 34,
                    lava: false,
                    lava_interval_ms: None,
                    generated: false,
                    mastery: false,
                }),
            },
            Level {
                id: "1-1".to_string(),
                title: "Count Crates",
                lesson: 1,
                required_to_unlock_next: false,
                kind: LevelKind::Grid(GridSpec {
                    width: 33,
                    height: 13,
                    start: Pos { x: 2, y: 6 },
                    goal: Pos { x: 30, y: 6 },
                    command_budget: 12,
                    lava: false,
                    lava_interval_ms: None,
                    generated: true,
                    mastery: false,
                }),
            },
            Level {
                id: "1-2".to_string(),
                title: "Rising Lava Vault",
                lesson: 1,
                required_to_unlock_next: true,
                kind: LevelKind::Grid(GridSpec {
                    width: 43,
                    height: 23,
                    start: Pos { x: 2, y: 20 },
                    goal: Pos { x: 40, y: 1 },
                    command_budget: 38,
                    lava: true,
                    lava_interval_ms: Some(3_400),
                    generated: true,
                    mastery: false,
                }),
            },
            Level {
                id: "1-3".to_string(),
                title: "Ember Sprint",
                lesson: 1,
                required_to_unlock_next: false,
                kind: LevelKind::Grid(GridSpec {
                    width: 57,
                    height: 29,
                    start: Pos { x: 2, y: 26 },
                    goal: Pos { x: 54, y: 1 },
                    command_budget: 48,
                    lava: true,
                    lava_interval_ms: Some(2_700),
                    generated: true,
                    mastery: true,
                }),
            },
        ],
    }
}

fn lesson_two() -> Lesson {
    Lesson {
        number: 2,
        title: "Word Rails: Code Navigation",
        commands: CommandSet::for_lesson(2),
        levels: vec![
            Level {
                id: "2-0".to_string(),
                title: "Tutorial: Token Tracks",
                lesson: 2,
                required_to_unlock_next: false,
                kind: LevelKind::Token(TokenSpec {
                    lines: vec![
                        "let courier_patch = old_bug.fix();",
                        "let route = courier_patch.next_stop();",
                        "dispatch(route);",
                    ],
                    targets: vec![(0, 4), (0, 18), (0, 28), (1, 12), (2, 0)],
                    command_budget: 18,
                    timer_seconds: None,
                    generated: false,
                }),
            },
            Level {
                id: "2-1".to_string(),
                title: "Symbol Switchyard",
                lesson: 2,
                required_to_unlock_next: false,
                kind: LevelKind::Token(TokenSpec {
                    lines: vec![
                        "route.alpha_beta.map(|fix| fix.apply())",
                        "packet.queue[fix.index()].seal();",
                    ],
                    targets: vec![(0, 6), (0, 17), (0, 28), (0, 37), (1, 7), (1, 20)],
                    command_budget: 20,
                    timer_seconds: None,
                    generated: true,
                }),
            },
            Level {
                id: "2-2".to_string(),
                title: "Refactor Route",
                lesson: 2,
                required_to_unlock_next: true,
                kind: LevelKind::Token(TokenSpec {
                    lines: vec![
                        "fn deliver_patch(queue: &mut Vec<Job>) {",
                        "    queue.iter_mut().for_each(|job| job.fix());",
                        "    report_status(queue.len());",
                        "}",
                    ],
                    targets: vec![(0, 3), (0, 17), (1, 10), (1, 32), (1, 42), (2, 4), (2, 18)],
                    command_budget: 28,
                    timer_seconds: None,
                    generated: true,
                }),
            },
            Level {
                id: "2-3".to_string(),
                title: "Stack Trace Chase",
                lesson: 2,
                required_to_unlock_next: false,
                kind: LevelKind::Token(TokenSpec {
                    lines: vec![
                        "panic: patch lost at src/courier.rs:42",
                        "  at deliver::route::vault",
                        "  at main::dispatch",
                        "  at runtime::wake_terminal",
                    ],
                    targets: vec![(0, 7), (0, 24), (0, 39), (1, 5), (1, 20), (2, 6), (3, 15)],
                    command_budget: 30,
                    timer_seconds: Some(75),
                    generated: true,
                }),
            },
        ],
    }
}

fn lesson_three() -> Lesson {
    Lesson {
        number: 3,
        title: "Code Surgery: Basic Editing",
        commands: CommandSet::for_lesson(3),
        levels: vec![
            Level {
                id: "3-0".to_string(),
                title: "Tutorial: First Patch",
                lesson: 3,
                required_to_unlock_next: false,
                kind: LevelKind::Edit(EditSpec {
                    start: vec![
                        "fn patch() {",
                        "    let mesage = \"ship\";",
                        "    let extra word = true;",
                        "}",
                    ],
                    expected: vec![
                        "fn patch() {",
                        "    let message = \"ship\";",
                        "    let extra = true;",
                        "}",
                    ],
                    cursor: Pos { x: 10, y: 1 },
                    command_budget: 42,
                    timer_seconds: None,
                }),
            },
            Level {
                id: "3-1".to_string(),
                title: "Bug Clinic",
                lesson: 3,
                required_to_unlock_next: false,
                kind: LevelKind::Edit(EditSpec {
                    start: vec![
                        "fn count_patch() -> usize {",
                        "    retun patch.count()",
                        "}",
                        "let mode = \"sloww\";",
                    ],
                    expected: vec![
                        "fn count_patch() -> usize {",
                        "    return patch.count()",
                        "}",
                        "let mode = \"slow\";",
                    ],
                    cursor: Pos { x: 8, y: 1 },
                    command_budget: 46,
                    timer_seconds: None,
                }),
            },
            Level {
                id: "3-2".to_string(),
                title: "Broken Build Bay",
                lesson: 3,
                required_to_unlock_next: true,
                kind: LevelKind::Edit(EditSpec {
                    start: vec![
                        "fn mainn() {",
                        "    prnitln!(\"patched\");",
                        "    let stale_word flag = true;",
                        "    dispatch_noww();",
                        "}",
                    ],
                    expected: vec![
                        "fn main() {",
                        "    println!(\"patched\");",
                        "    let flag = true;",
                        "    dispatch_now();",
                        "}",
                    ],
                    cursor: Pos { x: 7, y: 0 },
                    command_budget: 58,
                    timer_seconds: Some(180),
                }),
            },
            Level {
                id: "3-3".to_string(),
                title: "Undo Lab",
                lesson: 3,
                required_to_unlock_next: false,
                kind: LevelKind::Edit(EditSpec {
                    start: vec!["let target = \"wrong\";", "let backup = \"right\";"],
                    expected: vec!["let target = \"right\";", "let backup = \"right\";"],
                    cursor: Pos { x: 14, y: 0 },
                    command_budget: 38,
                    timer_seconds: None,
                }),
            },
        ],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CommandSet {
    lesson: u8,
}

impl CommandSet {
    fn for_lesson(lesson: u8) -> Self {
        Self { lesson }
    }

    fn allows_word(self) -> bool {
        self.lesson >= 2
    }

    fn allows_edit(self) -> bool {
        self.lesson >= 3
    }

    fn help(self) -> &'static str {
        match self.lesson {
            1 => "h j k l and counts: 3j, 5l",
            2 => "h j k l counts plus w b e ge",
            _ => "motions plus i a Esc x r<char> dw cw dd u",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedCommand {
    Motion(MotionKind, usize),
    InsertBefore,
    InsertAfter,
    Escape,
    DeleteChar,
    Replace(char),
    DeleteWord,
    ChangeWord,
    DeleteLine,
    Undo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MotionKind {
    Left,
    Down,
    Up,
    Right,
    WordForward,
    WordBackward,
    WordEndForward,
    WordEndBackward,
}

fn parse_command(input: &str, commands: CommandSet) -> std::result::Result<ParsedCommand, String> {
    if input == "<Esc>" {
        return Ok(ParsedCommand::Escape);
    }

    let mut chars = input.chars().peekable();
    let mut count = 0usize;
    while chars.peek().is_some_and(|ch| ch.is_ascii_digit()) {
        let digit = chars.next().unwrap().to_digit(10).unwrap() as usize;
        count = count.saturating_mul(10).saturating_add(digit);
    }
    let count = max(count, 1);
    let rest: String = chars.collect();

    match rest.as_str() {
        "h" => Ok(ParsedCommand::Motion(MotionKind::Left, count)),
        "j" => Ok(ParsedCommand::Motion(MotionKind::Down, count)),
        "k" => Ok(ParsedCommand::Motion(MotionKind::Up, count)),
        "l" => Ok(ParsedCommand::Motion(MotionKind::Right, count)),
        "w" if commands.allows_word() => Ok(ParsedCommand::Motion(MotionKind::WordForward, count)),
        "b" if commands.allows_word() => Ok(ParsedCommand::Motion(MotionKind::WordBackward, count)),
        "e" if commands.allows_word() => {
            Ok(ParsedCommand::Motion(MotionKind::WordEndForward, count))
        }
        "ge" if commands.allows_word() => {
            Ok(ParsedCommand::Motion(MotionKind::WordEndBackward, count))
        }
        "i" if commands.allows_edit() => Ok(ParsedCommand::InsertBefore),
        "a" if commands.allows_edit() => Ok(ParsedCommand::InsertAfter),
        "x" if commands.allows_edit() => Ok(ParsedCommand::DeleteChar),
        "dw" if commands.allows_edit() => Ok(ParsedCommand::DeleteWord),
        "cw" if commands.allows_edit() => Ok(ParsedCommand::ChangeWord),
        "dd" if commands.allows_edit() => Ok(ParsedCommand::DeleteLine),
        "u" if commands.allows_edit() => Ok(ParsedCommand::Undo),
        _ if rest.starts_with('r') && commands.allows_edit() && rest.chars().count() == 2 => {
            Ok(ParsedCommand::Replace(rest.chars().nth(1).unwrap()))
        }
        _ => Err(format!(
            "`{input}` is not in this lesson's command vocabulary"
        )),
    }
}

#[derive(Debug, Clone)]
struct GameSession {
    level: Level,
    commands: CommandSet,
    state: PlayState,
    input: String,
    accepted_commands: usize,
    start: Instant,
    finished: Option<LevelOutcome>,
    message: String,
    command_history: Vec<String>,
    moves_history: Vec<MotionKind>,
}

#[derive(Debug, Clone)]
enum PlayState {
    Grid(GridState),
    Token(TokenState),
    Edit(EditState),
}

#[derive(Debug, Clone)]
struct GridState {
    tiles: Vec<Vec<Tile>>,
    player: Pos,
    goal: Pos,
    command_budget: usize,
    lava_rows: usize,
    lava: bool,
    lava_interval: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tile {
    Floor,
    Wall,
}

#[derive(Debug, Clone)]
struct TokenState {
    lines: Vec<String>,
    cursor: Pos,
    targets: Vec<Pos>,
    next_target: usize,
    command_budget: usize,
    timer_seconds: Option<u64>,
}

#[derive(Debug, Clone)]
struct EditState {
    lines: Vec<String>,
    expected: Vec<String>,
    cursor: Pos,
    mode: EditorMode,
    undo: Vec<Vec<String>>,
    command_budget: usize,
    timer_seconds: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorMode {
    Normal,
    Insert,
    ChangeWord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LevelResult {
    Clear,
    Defeat,
    Quit,
}

#[derive(Debug, Clone)]
struct LevelOutcome {
    level_id: String,
    result: LevelResult,
    elapsed: Duration,
    commands: usize,
    message: String,
    hint: Option<String>,
}

impl LevelOutcome {
    fn summary(&self) -> String {
        let status = match self.result {
            LevelResult::Clear => "clear",
            LevelResult::Defeat => "defeat",
            LevelResult::Quit => "quit",
        };
        let hint = self
            .hint
            .as_ref()
            .map_or(String::new(), |hint| format!("\nHint: {hint}"));
        format!(
            "{}: {} in {:.1}s using {} commands. {}{}",
            self.level_id,
            status,
            self.elapsed.as_secs_f32(),
            self.commands,
            self.message,
            hint
        )
    }
}

impl GameSession {
    fn new(level: &Level, seed: u64) -> Self {
        let commands = CommandSet::for_lesson(level.lesson);
        let state = match &level.kind {
            LevelKind::Grid(spec) => PlayState::Grid(GridState::from_spec(spec, seed)),
            LevelKind::Token(spec) => PlayState::Token(TokenState::from_spec(spec, seed)),
            LevelKind::Edit(spec) => PlayState::Edit(EditState::from_spec(spec)),
        };
        Self {
            level: level.clone(),
            commands,
            state,
            input: String::new(),
            accepted_commands: 0,
            start: Instant::now(),
            finished: None,
            message: "Deliver the patch. Type a Vim command; q quits the level.".to_string(),
            command_history: Vec::new(),
            moves_history: Vec::new(),
        }
    }

    fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    fn on_key(&mut self, key: KeyEvent) {
        if self.finished.is_some() {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.finish(LevelResult::Quit, "Interrupted.".to_string());
            return;
        }

        if matches!(
            self.state,
            PlayState::Edit(EditState {
                mode: EditorMode::Insert | EditorMode::ChangeWord,
                ..
            })
        ) {
            self.handle_insert_key(key);
            return;
        }

        match key.code {
            KeyCode::Esc => {
                if self.input.is_empty() {
                    self.finish(LevelResult::Quit, "Back to the dispatch board.".to_string());
                } else {
                    self.input.clear();
                    self.message = "Command buffer cleared.".to_string();
                }
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Enter => {
                if !self.input.is_empty() {
                    self.execute_buffer();
                }
            }
            KeyCode::Char('q') if self.input.is_empty() => {
                self.finish(LevelResult::Quit, "Back to the dispatch board.".to_string());
            }
            KeyCode::Char(ch) => {
                self.input.push(ch);
                if self.buffer_is_complete() {
                    self.execute_buffer();
                }
            }
            _ => {}
        }
    }

    fn handle_insert_key(&mut self, key: KeyEvent) {
        let mut leave_insert = false;
        let mut accepted = false;
        if let PlayState::Edit(edit) = &mut self.state {
            match key.code {
                KeyCode::Esc => {
                    leave_insert = true;
                    edit.mode = EditorMode::Normal;
                    edit.cursor.x = edit.cursor.x.saturating_sub(1);
                }
                KeyCode::Backspace => {
                    if edit.cursor.x > 0 {
                        let y = edit.cursor.y;
                        let idx = byte_index_at_char(&edit.lines[y], edit.cursor.x - 1);
                        edit.lines[y].remove(idx);
                        edit.cursor.x -= 1;
                        accepted = true;
                    }
                }
                KeyCode::Char(ch) => {
                    let y = edit.cursor.y;
                    let idx = byte_index_at_char(&edit.lines[y], edit.cursor.x);
                    edit.lines[y].insert(idx, ch);
                    edit.cursor.x += 1;
                    accepted = true;
                }
                _ => {}
            }
        }

        if accepted {
            self.accepted_commands += 1;
            self.command_history.push("insert-char".to_string());
            self.check_limits();
            self.check_edit_clear();
        }
        if leave_insert {
            self.message = "Normal mode.".to_string();
        }
    }

    fn buffer_is_complete(&self) -> bool {
        if self.input.chars().all(|ch| ch.is_ascii_digit()) {
            return false;
        }
        let commands = self.commands;
        let bare = self
            .input
            .trim_start_matches(|ch: char| ch.is_ascii_digit())
            .to_string();
        match bare.as_str() {
            "h" | "j" | "k" | "l" => true,
            "w" | "b" | "e" => commands.allows_word(),
            "ge" => commands.allows_word(),
            "i" | "a" | "x" | "u" | "dw" | "cw" | "dd" => commands.allows_edit(),
            _ if bare.starts_with('r') && bare.chars().count() == 2 => commands.allows_edit(),
            _ => false,
        }
    }

    fn execute_buffer(&mut self) {
        let input = std::mem::take(&mut self.input);
        match parse_command(&input, self.commands) {
            Ok(command) => {
                self.command_history.push(input);
                self.accepted_commands += 1;
                self.apply_command(command);
                self.check_limits();
            }
            Err(err) => {
                self.message = err;
            }
        }
    }

    fn apply_command(&mut self, command: ParsedCommand) {
        match (&mut self.state, command) {
            (PlayState::Grid(grid), ParsedCommand::Motion(kind, count)) => {
                self.moves_history.extend(std::iter::repeat_n(kind, count));
                grid.apply_motion(kind, count);
                if grid.player == grid.goal {
                    self.finish(
                        LevelResult::Clear,
                        "Patch delivered through the hot floor.".to_string(),
                    );
                } else if grid.player_in_lava() {
                    self.finish(
                        LevelResult::Defeat,
                        "The lava reached your boots.".to_string(),
                    );
                } else {
                    self.message = describe_motion(kind, count);
                }
            }
            (PlayState::Token(token), ParsedCommand::Motion(kind, count)) => {
                self.moves_history.extend(std::iter::repeat_n(kind, count));
                token.apply_motion(kind, count);
                token.collect_targets();
                if token.next_target == token.targets.len() {
                    self.finish(
                        LevelResult::Clear,
                        "You rode the word rails cleanly.".to_string(),
                    );
                } else {
                    self.message = format!(
                        "{} Target {}/{}.",
                        describe_motion(kind, count),
                        token.next_target + 1,
                        token.targets.len()
                    );
                }
            }
            (PlayState::Edit(edit), ParsedCommand::Motion(kind, count)) => {
                self.moves_history.extend(std::iter::repeat_n(kind, count));
                edit.apply_motion(kind, count);
                self.message = describe_motion(kind, count);
            }
            (PlayState::Edit(edit), ParsedCommand::InsertBefore) => {
                edit.push_undo();
                edit.mode = EditorMode::Insert;
                self.message = "-- INSERT --".to_string();
            }
            (PlayState::Edit(edit), ParsedCommand::InsertAfter) => {
                edit.push_undo();
                edit.cursor.x = min(edit.cursor.x + 1, edit.line_len(edit.cursor.y));
                edit.mode = EditorMode::Insert;
                self.message = "-- INSERT --".to_string();
            }
            (PlayState::Edit(edit), ParsedCommand::DeleteChar) => {
                edit.push_undo();
                edit.delete_char();
                self.message = "Deleted one character with x.".to_string();
                self.check_edit_clear();
            }
            (PlayState::Edit(edit), ParsedCommand::Replace(ch)) => {
                edit.push_undo();
                edit.replace_char(ch);
                self.message = format!("Replaced one character with {ch}.");
                self.check_edit_clear();
            }
            (PlayState::Edit(edit), ParsedCommand::DeleteWord) => {
                edit.push_undo();
                edit.delete_word();
                self.message = "Deleted a word with dw.".to_string();
                self.check_edit_clear();
            }
            (PlayState::Edit(edit), ParsedCommand::ChangeWord) => {
                edit.push_undo();
                edit.delete_word();
                edit.mode = EditorMode::ChangeWord;
                self.message = "-- CHANGE WORD --".to_string();
            }
            (PlayState::Edit(edit), ParsedCommand::DeleteLine) => {
                edit.push_undo();
                edit.delete_line();
                self.message = "Deleted a line with dd.".to_string();
                self.check_edit_clear();
            }
            (PlayState::Edit(edit), ParsedCommand::Undo) => {
                if edit.undo_last() {
                    self.message = "Undo restored the previous buffer.".to_string();
                } else {
                    self.message = "Nothing to undo yet.".to_string();
                }
                self.check_edit_clear();
            }
            (
                _,
                ParsedCommand::InsertBefore
                | ParsedCommand::InsertAfter
                | ParsedCommand::DeleteChar
                | ParsedCommand::Replace(_)
                | ParsedCommand::DeleteWord
                | ParsedCommand::ChangeWord
                | ParsedCommand::DeleteLine
                | ParsedCommand::Undo,
            ) => {
                self.message = "That edit command belongs in Code Surgery levels.".to_string();
            }
            (_, ParsedCommand::Escape) => {}
        }
    }

    fn check_edit_clear(&mut self) {
        if let PlayState::Edit(edit) = &self.state
            && edit.mode == EditorMode::Normal
            && edit.lines == edit.expected
        {
            self.finish(
                LevelResult::Clear,
                "The patch compiles in the bay.".to_string(),
            );
        }
    }

    fn check_limits(&mut self) {
        if self.finished.is_some() {
            return;
        }
        self.update_timed_hazards();
        if let PlayState::Grid(grid) = &self.state
            && grid.player_in_lava()
        {
            self.finish(
                LevelResult::Defeat,
                "The timed lava front caught the courier.".to_string(),
            );
            return;
        }
        match &self.state {
            PlayState::Grid(grid) if self.accepted_commands >= grid.command_budget => {
                self.finish(LevelResult::Defeat, "Command budget spent.".to_string());
            }
            PlayState::Token(token) if self.accepted_commands >= token.command_budget => {
                self.finish(LevelResult::Defeat, "Command budget spent.".to_string());
            }
            PlayState::Token(token)
                if token
                    .timer_seconds
                    .is_some_and(|limit| self.elapsed().as_secs() >= limit) =>
            {
                self.finish(
                    LevelResult::Defeat,
                    "The trace cooled before you arrived.".to_string(),
                );
            }
            PlayState::Edit(edit) if self.accepted_commands >= edit.command_budget => {
                self.finish(
                    LevelResult::Defeat,
                    "Command budget spent before the patch matched.".to_string(),
                );
            }
            PlayState::Edit(edit)
                if edit
                    .timer_seconds
                    .is_some_and(|limit| self.elapsed().as_secs() >= limit) =>
            {
                self.finish(LevelResult::Defeat, "Build bay timer expired.".to_string());
            }
            _ => {}
        }
    }

    fn update_timed_hazards(&mut self) {
        let elapsed = self.elapsed();
        if let PlayState::Grid(grid) = &mut self.state {
            grid.update_lava(elapsed);
        }
    }

    fn finish(&mut self, result: LevelResult, message: String) {
        let hint = (result == LevelResult::Defeat)
            .then(|| self.hint())
            .flatten();
        self.finished = Some(LevelOutcome {
            level_id: self.level.id.clone(),
            result,
            elapsed: self.elapsed(),
            commands: self.accepted_commands,
            message: message.clone(),
            hint,
        });
        self.message = message;
    }

    fn hint(&self) -> Option<String> {
        match &self.state {
            PlayState::Grid(grid) => hint_for_counts(&self.moves_history).or_else(|| {
                grid.lava.then(|| {
                    "Lava rises by real time now. Pre-plan long motions like `6k` or `8l` instead of waiting between single steps.".to_string()
                })
            }),
            PlayState::Token(_) => hint_for_words(&self.command_history),
            PlayState::Edit(edit) => hint_for_edits(edit),
        }
    }
}

fn describe_motion(kind: MotionKind, count: usize) -> String {
    let command = match kind {
        MotionKind::Left => "h",
        MotionKind::Down => "j",
        MotionKind::Up => "k",
        MotionKind::Right => "l",
        MotionKind::WordForward => "w",
        MotionKind::WordBackward => "b",
        MotionKind::WordEndForward => "e",
        MotionKind::WordEndBackward => "ge",
    };
    if count > 1 {
        format!("{count}{command} moved {count} steps.")
    } else {
        format!("{command} moved once.")
    }
}

impl GridState {
    fn from_spec(spec: &GridSpec, seed: u64) -> Self {
        let mut tiles = vec![vec![Tile::Floor; spec.width]; spec.height];
        for (y, row) in tiles.iter_mut().enumerate().take(spec.height) {
            for (x, tile) in row.iter_mut().enumerate().take(spec.width) {
                if x == 0 || y == 0 || x == spec.width - 1 || y == spec.height - 1 {
                    *tile = Tile::Wall;
                }
            }
        }

        if !spec.generated {
            if let Some(row) = tiles.get_mut(3) {
                for (x, tile) in row
                    .iter_mut()
                    .enumerate()
                    .take(spec.width.saturating_sub(3))
                    .skip(3)
                {
                    if x != 6 {
                        *tile = Tile::Wall;
                    }
                }
            }
        } else {
            carve_count_friendly_maze(&mut tiles, spec.start, spec.goal, seed, spec.mastery);
        }

        tiles[spec.start.y][spec.start.x] = Tile::Floor;
        tiles[spec.goal.y][spec.goal.x] = Tile::Floor;

        Self {
            tiles,
            player: spec.start,
            goal: spec.goal,
            command_budget: spec.command_budget,
            lava_rows: usize::from(spec.lava),
            lava: spec.lava,
            lava_interval: spec.lava_interval_ms.map(Duration::from_millis),
        }
    }

    fn update_lava(&mut self, elapsed: Duration) {
        if let Some(interval) = self.lava_interval {
            let interval_ms = interval.as_millis().max(1);
            let elapsed_rows = elapsed.as_millis() / interval_ms;
            self.lava_rows = min(
                1 + elapsed_rows as usize,
                self.tiles.len().saturating_sub(1),
            );
        }
    }

    fn player_in_lava(&self) -> bool {
        self.lava && self.player.y >= self.tiles.len().saturating_sub(self.lava_rows)
    }

    fn lava_status(&self, elapsed: Duration) -> Option<String> {
        self.lava_interval.map(|interval| {
            let interval_ms = interval.as_millis().max(1);
            let elapsed_ms = elapsed.as_millis();
            let remaining_ms = interval_ms - (elapsed_ms % interval_ms);
            format!(
                " | Lava: {} rows, next rise in {:.1}s",
                self.lava_rows,
                remaining_ms as f32 / 1000.0
            )
        })
    }

    fn apply_motion(&mut self, kind: MotionKind, count: usize) {
        for _ in 0..count {
            let next = match kind {
                MotionKind::Left => Pos {
                    x: self.player.x.saturating_sub(1),
                    y: self.player.y,
                },
                MotionKind::Right => Pos {
                    x: self.player.x + 1,
                    y: self.player.y,
                },
                MotionKind::Up => Pos {
                    x: self.player.x,
                    y: self.player.y.saturating_sub(1),
                },
                MotionKind::Down => Pos {
                    x: self.player.x,
                    y: self.player.y + 1,
                },
                MotionKind::WordForward
                | MotionKind::WordBackward
                | MotionKind::WordEndForward
                | MotionKind::WordEndBackward => self.player,
            };
            if self.is_floor(next) {
                self.player = next;
            } else {
                break;
            }
        }
    }

    fn is_floor(&self, pos: Pos) -> bool {
        self.tiles
            .get(pos.y)
            .and_then(|row| row.get(pos.x))
            .is_some_and(|tile| *tile == Tile::Floor)
    }
}

fn carve_count_friendly_maze(
    tiles: &mut [Vec<Tile>],
    start: Pos,
    goal: Pos,
    seed: u64,
    mastery: bool,
) {
    let height = tiles.len();
    for row in tiles.iter_mut().skip(1).take(height.saturating_sub(2)) {
        let width = row.len();
        for tile in row.iter_mut().skip(1).take(width.saturating_sub(2)) {
            *tile = Tile::Wall;
        }
    }

    let mut rng = Lcg::new(seed);
    let mut current = start;
    tiles[current.y][current.x] = Tile::Floor;
    while current.y > goal.y {
        let vertical = min(current.y - goal.y, 2 + rng.range(4));
        for _ in 0..vertical {
            current.y -= 1;
            tiles[current.y][current.x] = Tile::Floor;
        }
        let target_x = if current.y <= goal.y + 1 {
            goal.x
        } else {
            1 + rng.range(tiles[0].len().saturating_sub(2))
        };
        while current.x != target_x {
            current.x = if current.x < target_x {
                current.x + 1
            } else {
                current.x - 1
            };
            tiles[current.y][current.x] = Tile::Floor;
        }
    }
    while current.x != goal.x {
        current.x = if current.x < goal.x {
            current.x + 1
        } else {
            current.x - 1
        };
        tiles[current.y][current.x] = Tile::Floor;
    }
    tiles[goal.y][goal.x] = Tile::Floor;

    let side_rooms = if mastery { 16 } else { 8 };
    for _ in 0..side_rooms {
        let y = 1 + rng.range(tiles.len().saturating_sub(2));
        let x = 1 + rng.range(tiles[0].len().saturating_sub(2));
        if tiles[y][x] == Tile::Floor {
            let len = 1 + rng.range(4);
            let right = rng.next().is_multiple_of(2);
            let mut cx = x;
            for _ in 0..len {
                cx = if right {
                    min(cx + 1, tiles[0].len() - 2)
                } else {
                    cx.saturating_sub(1).max(1)
                };
                tiles[y][cx] = Tile::Floor;
            }
        }
    }
}

impl TokenState {
    fn from_spec(spec: &TokenSpec, seed: u64) -> Self {
        let mut lines: Vec<String> = spec.lines.iter().map(|line| (*line).to_string()).collect();
        if spec.generated {
            decorate_generated_tokens(&mut lines, seed);
        }
        let mut state = Self {
            lines,
            cursor: Pos { x: 0, y: 0 },
            targets: spec
                .targets
                .iter()
                .map(|(y, x)| Pos { x: *x, y: *y })
                .collect(),
            next_target: 0,
            command_budget: spec.command_budget,
            timer_seconds: spec.timer_seconds,
        };
        state.collect_targets();
        state
    }

    fn apply_motion(&mut self, kind: MotionKind, count: usize) {
        for _ in 0..count {
            match kind {
                MotionKind::Left => self.cursor = self.left_pos(self.cursor),
                MotionKind::Right => self.cursor = self.right_pos(self.cursor),
                MotionKind::Up => self.cursor = self.vertical(-1),
                MotionKind::Down => self.cursor = self.vertical(1),
                MotionKind::WordForward => self.cursor = self.word_forward(false),
                MotionKind::WordBackward => self.cursor = self.word_backward(false),
                MotionKind::WordEndForward => self.cursor = self.word_forward(true),
                MotionKind::WordEndBackward => self.cursor = self.word_backward(true),
            }
        }
    }

    fn collect_targets(&mut self) {
        while self
            .targets
            .get(self.next_target)
            .is_some_and(|target| *target == self.cursor)
        {
            self.next_target += 1;
        }
    }

    fn left_pos(&self, pos: Pos) -> Pos {
        if pos.x > 0 {
            Pos {
                x: pos.x - 1,
                y: pos.y,
            }
        } else if pos.y > 0 {
            Pos {
                x: self.line_len(pos.y - 1).saturating_sub(1),
                y: pos.y - 1,
            }
        } else {
            pos
        }
    }

    fn right_pos(&self, pos: Pos) -> Pos {
        if pos.x + 1 < self.line_len(pos.y) {
            Pos {
                x: pos.x + 1,
                y: pos.y,
            }
        } else if pos.y + 1 < self.lines.len() {
            Pos { x: 0, y: pos.y + 1 }
        } else {
            pos
        }
    }

    fn vertical(&self, delta: isize) -> Pos {
        let y = if delta < 0 {
            self.cursor.y.saturating_sub(delta.unsigned_abs())
        } else {
            min(
                self.cursor.y + delta as usize,
                self.lines.len().saturating_sub(1),
            )
        };
        Pos {
            x: min(self.cursor.x, self.line_len(y).saturating_sub(1)),
            y,
        }
    }

    fn line_len(&self, y: usize) -> usize {
        self.lines
            .get(y)
            .map_or(0, |line| line.chars().count())
            .max(1)
    }

    fn word_forward(&self, end: bool) -> Pos {
        let mut pos = self.right_pos(self.cursor);
        while !self.is_word_char(pos) && pos != self.right_pos(pos) {
            pos = self.right_pos(pos);
        }
        if end {
            while self.is_word_char(self.right_pos(pos)) && self.right_pos(pos) != pos {
                pos = self.right_pos(pos);
            }
        }
        pos
    }

    fn word_backward(&self, end: bool) -> Pos {
        let mut pos = self.left_pos(self.cursor);
        while !self.is_word_char(pos) && pos != self.left_pos(pos) {
            pos = self.left_pos(pos);
        }
        if end {
            while self.is_word_char(self.left_pos(pos)) && self.left_pos(pos) != pos {
                pos = self.left_pos(pos);
            }
            while self.is_word_char(self.right_pos(pos)) && self.right_pos(pos) != pos {
                pos = self.right_pos(pos);
            }
        } else {
            while self.is_word_char(self.left_pos(pos)) && self.left_pos(pos) != pos {
                pos = self.left_pos(pos);
            }
        }
        pos
    }

    fn is_word_char(&self, pos: Pos) -> bool {
        self.lines
            .get(pos.y)
            .and_then(|line| line.chars().nth(pos.x))
            .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    }
}

fn decorate_generated_tokens(lines: &mut [String], seed: u64) {
    let mut rng = Lcg::new(seed);
    let suffixes = ["_hot", "_ok", "_v2", "_fast"];
    for line in lines {
        if rng.next().is_multiple_of(2) {
            line.push_str(suffixes[rng.range(suffixes.len())]);
        }
    }
}

impl EditState {
    fn from_spec(spec: &EditSpec) -> Self {
        Self {
            lines: spec.start.iter().map(|line| (*line).to_string()).collect(),
            expected: spec
                .expected
                .iter()
                .map(|line| (*line).to_string())
                .collect(),
            cursor: spec.cursor,
            mode: EditorMode::Normal,
            undo: Vec::new(),
            command_budget: spec.command_budget,
            timer_seconds: spec.timer_seconds,
        }
    }

    fn line_len(&self, y: usize) -> usize {
        self.lines.get(y).map_or(0, |line| line.chars().count())
    }

    fn push_undo(&mut self) {
        self.undo.push(self.lines.clone());
        if self.undo.len() > 24 {
            self.undo.remove(0);
        }
    }

    fn undo_last(&mut self) -> bool {
        if let Some(lines) = self.undo.pop() {
            self.lines = lines;
            self.cursor.x = min(
                self.cursor.x,
                self.line_len(self.cursor.y).saturating_sub(1),
            );
            true
        } else {
            false
        }
    }

    fn apply_motion(&mut self, kind: MotionKind, count: usize) {
        let mut token = TokenState {
            lines: self.lines.clone(),
            cursor: self.cursor,
            targets: Vec::new(),
            next_target: 0,
            command_budget: self.command_budget,
            timer_seconds: self.timer_seconds,
        };
        token.apply_motion(kind, count);
        self.cursor = token.cursor;
    }

    fn delete_char(&mut self) {
        if self.cursor.y >= self.lines.len() || self.cursor.x >= self.line_len(self.cursor.y) {
            return;
        }
        let idx = byte_index_at_char(&self.lines[self.cursor.y], self.cursor.x);
        self.lines[self.cursor.y].remove(idx);
        self.cursor.x = min(
            self.cursor.x,
            self.line_len(self.cursor.y).saturating_sub(1),
        );
    }

    fn replace_char(&mut self, ch: char) {
        if self.cursor.y >= self.lines.len() || self.cursor.x >= self.line_len(self.cursor.y) {
            return;
        }
        let idx = byte_index_at_char(&self.lines[self.cursor.y], self.cursor.x);
        self.lines[self.cursor.y].remove(idx);
        self.lines[self.cursor.y].insert(idx, ch);
    }

    fn delete_word(&mut self) {
        if self.cursor.y >= self.lines.len() {
            return;
        }
        let y = self.cursor.y;
        let line = self.lines[y].clone();
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            return;
        }
        let mut start = min(self.cursor.x, chars.len().saturating_sub(1));
        while start < chars.len() && !is_word_char(chars[start]) {
            start += 1;
        }
        if start >= chars.len() {
            return;
        }
        let mut end = start;
        while end < chars.len() && is_word_char(chars[end]) {
            end += 1;
        }
        let start_idx = byte_index_at_char(&self.lines[y], start);
        let end_idx = byte_index_at_char(&self.lines[y], end);
        self.lines[y].replace_range(start_idx..end_idx, "");
        self.cursor.x = min(start, self.line_len(y).saturating_sub(1));
    }

    fn delete_line(&mut self) {
        if self.lines.len() <= 1 {
            self.lines[0].clear();
            self.cursor = Pos { x: 0, y: 0 };
            return;
        }
        self.lines.remove(self.cursor.y);
        self.cursor.y = min(self.cursor.y, self.lines.len() - 1);
        self.cursor.x = min(
            self.cursor.x,
            self.line_len(self.cursor.y).saturating_sub(1),
        );
    }
}

fn is_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn byte_index_at_char(s: &str, char_index: usize) -> usize {
    s.char_indices()
        .nth(char_index)
        .map_or_else(|| s.len(), |(idx, _)| idx)
}

fn hint_for_counts(moves: &[MotionKind]) -> Option<String> {
    let mut best_kind = None;
    let mut best_len = 0usize;
    let mut current_kind = None;
    let mut current_len = 0usize;
    for &kind in moves {
        if Some(kind) == current_kind {
            current_len += 1;
        } else {
            current_kind = Some(kind);
            current_len = 1;
        }
        if current_len > best_len {
            best_len = current_len;
            best_kind = Some(kind);
        }
    }
    (best_len >= 3).then(|| {
        let key = match best_kind.unwrap() {
            MotionKind::Left => "h",
            MotionKind::Down => "j",
            MotionKind::Up => "k",
            MotionKind::Right => "l",
            _ => "?",
        };
        format!("You repeated `{key}` {best_len} times. Try `{best_len}{key}` as one command.")
    })
}

fn hint_for_words(history: &[String]) -> Option<String> {
    let repeated_chars = history
        .iter()
        .filter(|command| matches!(command.as_str(), "h" | "l"))
        .count();
    (repeated_chars >= 5).then(|| {
        "You crossed token text with h/l. Try `w`, `e`, `b`, or `ge` for code-token jumps."
            .to_string()
    })
}

fn hint_for_edits(edit: &EditState) -> Option<String> {
    if edit.lines == edit.expected {
        return None;
    }
    Some("Compare the buffer to the target: `x`, `r<char>`, `dw`, `cw`, `dd`, and `u` are available here.".to_string())
}

#[derive(Debug, Clone)]
struct Lcg {
    state: u64,
}

impl Lcg {
    fn new(seed: u64) -> Self {
        Self { state: seed.max(1) }
    }

    fn next(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.state
    }

    fn range(&mut self, upper: usize) -> usize {
        if upper == 0 {
            0
        } else {
            (self.next() as usize) % upper
        }
    }
}

fn run_single_level_cli(
    catalog: &Catalog,
    progress: &mut Progress,
    level_id: &str,
    seed: u64,
) -> Result<LevelOutcome> {
    let level = catalog
        .level(level_id)
        .with_context(|| format!("unknown level `{level_id}`"))?;
    if !progress.is_unlocked(&level.id) {
        anyhow::bail!("level `{}` is locked", level.id);
    }

    let mut session = GameSession::new(level, seed);
    run_terminal_level(&mut session)?;
    let outcome = if let Some(outcome) = session.finished.take() {
        outcome
    } else {
        LevelOutcome {
            level_id: level.id.clone(),
            result: LevelResult::Quit,
            elapsed: session.elapsed(),
            commands: session.accepted_commands,
            message: "Level closed.".to_string(),
            hint: None,
        }
    };
    if outcome.result == LevelResult::Clear {
        progress.record_clear(&level.id, outcome.elapsed, outcome.commands);
    }
    Ok(outcome)
}

type Term = Terminal<CrosstermBackend<Stdout>>;

fn run_tui(catalog: Catalog, mut progress: Progress, seed: u64) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = App::new(catalog, progress.clone(), seed).run(&mut terminal);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    match result {
        Ok(updated) => {
            progress = updated;
            progress.save()
        }
        Err(err) => Err(err),
    }
}

fn run_terminal_level(session: &mut GameSession) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = loop {
        terminal.draw(|frame| draw_level(frame, session, None))?;
        if event::poll(Duration::from_millis(120))?
            && let Event::Key(key) = event::read()?
        {
            session.on_key(key);
        }
        session.check_limits();
        if session.finished.is_some() {
            terminal.draw(|frame| draw_level(frame, session, None))?;
            break Ok(());
        }
    };
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Home,
    Lessons,
    Help,
    Practice,
    Settings,
    Level,
}

struct App {
    catalog: Catalog,
    progress: Progress,
    screen: Screen,
    selected_home: usize,
    selected_level: usize,
    seed: u64,
    session: Option<GameSession>,
    banner: String,
}

impl App {
    fn new(catalog: Catalog, progress: Progress, seed: u64) -> Self {
        Self {
            catalog,
            progress,
            screen: Screen::Home,
            selected_home: 0,
            selected_level: 0,
            seed,
            session: None,
            banner: "Patch Courier dispatch is online.".to_string(),
        }
    }

    fn run(mut self, terminal: &mut Term) -> Result<Progress> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;
            if event::poll(Duration::from_millis(120))?
                && let Event::Key(key) = event::read()?
                && self.handle_key(key)
            {
                return Ok(self.progress);
            }
            if let Some(session) = &mut self.session {
                session.check_limits();
                if let Some(outcome) = session.finished.clone() {
                    if outcome.result == LevelResult::Clear {
                        self.progress.record_clear(
                            &outcome.level_id,
                            outcome.elapsed,
                            outcome.commands,
                        );
                    }
                    self.banner = outcome.summary();
                    self.session = None;
                    self.screen = Screen::Lessons;
                }
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame<'_>) {
        match self.screen {
            Screen::Home => draw_home(frame, self),
            Screen::Lessons => draw_lessons(frame, self),
            Screen::Help => draw_help(frame, self),
            Screen::Practice => draw_practice(frame, self),
            Screen::Settings => draw_settings(frame, self),
            Screen::Level => {
                if let Some(session) = &self.session {
                    draw_level(frame, session, Some("Esc/q quits the level"));
                }
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        if self.screen == Screen::Level {
            if let Some(session) = &mut self.session {
                session.on_key(key);
            }
            return false;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Char('h') => self.screen = Screen::Home,
            KeyCode::Char('H') => self.screen = Screen::Help,
            KeyCode::Char('l') => self.screen = Screen::Lessons,
            KeyCode::Char('p') => self.screen = Screen::Practice,
            KeyCode::Char('s') => self.screen = Screen::Settings,
            KeyCode::Char('j') | KeyCode::Down if self.progress.menu_arrows => self.select_next(),
            KeyCode::Char('k') | KeyCode::Up if self.progress.menu_arrows => self.select_previous(),
            KeyCode::Enter => self.activate(),
            KeyCode::Char('r') if self.screen == Screen::Settings => {
                self.progress = Progress::new();
                self.banner = "Progress reset locally. It will save when you quit.".to_string();
            }
            KeyCode::Char('c') if self.screen == Screen::Settings => {
                self.progress.color_intense = !self.progress.color_intense;
            }
            KeyCode::Char('a') if self.screen == Screen::Settings => {
                self.progress.menu_arrows = !self.progress.menu_arrows;
            }
            _ => {}
        }
        false
    }

    fn select_next(&mut self) {
        match self.screen {
            Screen::Home => self.selected_home = (self.selected_home + 1) % 5,
            Screen::Lessons | Screen::Practice => {
                let total = self.catalog.all_levels().len();
                self.selected_level = (self.selected_level + 1) % total;
            }
            _ => {}
        }
    }

    fn select_previous(&mut self) {
        match self.screen {
            Screen::Home => self.selected_home = (self.selected_home + 4) % 5,
            Screen::Lessons | Screen::Practice => {
                let total = self.catalog.all_levels().len();
                self.selected_level = (self.selected_level + total - 1) % total;
            }
            _ => {}
        }
    }

    fn activate(&mut self) {
        match self.screen {
            Screen::Home => match self.selected_home {
                0 => {
                    if let Some(level) = self.catalog.next_recommended(&self.progress).cloned() {
                        self.start_level(&level);
                    }
                }
                1 => self.screen = Screen::Lessons,
                2 => self.screen = Screen::Help,
                3 => self.screen = Screen::Practice,
                4 => self.screen = Screen::Settings,
                _ => {}
            },
            Screen::Lessons | Screen::Practice => {
                if let Some(level) = self
                    .catalog
                    .all_levels()
                    .get(self.selected_level)
                    .copied()
                    .cloned()
                {
                    if self.progress.is_unlocked(&level.id) {
                        self.start_level(&level);
                    } else {
                        self.banner = format!(
                            "{} is locked. Clear the previous x-2 courier route first.",
                            level.id
                        );
                    }
                }
            }
            _ => {}
        }
    }

    fn start_level(&mut self, level: &Level) {
        self.progress.last_played = Some(level.id.clone());
        self.session = Some(GameSession::new(level, self.seed));
        self.screen = Screen::Level;
    }
}

fn draw_home(frame: &mut Frame<'_>, app: &App) {
    let chunks = page_layout(frame.area());
    render_title(frame, chunks[0], "Gamevim: Patch Courier", &app.banner);
    let items = ["Continue", "Lessons", "Help", "Practice", "Settings"];
    let rows: Vec<ListItem> = items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let marker = if idx == app.selected_home { "> " } else { "  " };
            ListItem::new(format!("{marker}{item}"))
        })
        .collect();
    frame.render_widget(
        List::new(rows).block(Block::default().title("Dispatch").borders(Borders::ALL)),
        chunks[1],
    );
    frame.render_widget(
        Paragraph::new("Enter starts a selection. h/j/k/l navigate menus; q leaves the app.")
            .block(Block::default().title("Controls").borders(Borders::ALL)),
        chunks[2],
    );
}

fn draw_lessons(frame: &mut Frame<'_>, app: &App) {
    draw_level_list(
        frame,
        app,
        "Lessons",
        "Enter starts any unlocked level. Required x-2 clears unlock the next lesson.",
    );
}

fn draw_practice(frame: &mut Frame<'_>, app: &App) {
    draw_level_list(
        frame,
        app,
        "Practice",
        "Replay unlocked levels with the current seed. Use --seed for quick CLI seeded replays.",
    );
}

fn draw_level_list(frame: &mut Frame<'_>, app: &App, title: &str, footer: &str) {
    let chunks = page_layout(frame.area());
    render_title(frame, chunks[0], title, &app.banner);
    let all = app.catalog.all_levels();
    let rows: Vec<ListItem> = all
        .iter()
        .enumerate()
        .map(|(idx, level)| {
            let marker = if idx == app.selected_level {
                "> "
            } else {
                "  "
            };
            let status = level_status(level, &app.progress);
            let required = if level.required_to_unlock_next {
                " unlock"
            } else {
                ""
            };
            let score = app
                .progress
                .levels
                .get(&level.id)
                .and_then(|p| p.best_commands)
                .map_or(String::new(), |best| format!(" best:{best}"));
            ListItem::new(format!(
                "{marker}{:<3} {:<27} [{status}{required}]{score}",
                level.id, level.title
            ))
        })
        .collect();
    frame.render_widget(
        List::new(rows).block(Block::default().title("Routes").borders(Borders::ALL)),
        chunks[1],
    );
    frame.render_widget(
        Paragraph::new(footer).block(Block::default().title("Note").borders(Borders::ALL)),
        chunks[2],
    );
}

fn draw_help(frame: &mut Frame<'_>, app: &App) {
    let chunks = page_layout(frame.area());
    render_title(frame, chunks[0], "Help", &app.banner);
    let unlocked_lesson = if app.progress.is_cleared("2-2") {
        3
    } else if app.progress.is_cleared("1-2") {
        2
    } else {
        1
    };
    let commands = CommandSet::for_lesson(unlocked_lesson).help();
    let text = format!(
        "Current vocabulary: {commands}\n\nUnlock rules: all Lesson 1 routes are open at first. Clear 1-2 to unlock Lesson 2. Clear 2-2 to unlock Lesson 3.\n\nHints react to defeats: repeated h/j/k/l suggests counts; h/l token crawling suggests word motions; edit mismatches suggest the relevant edit family.\n\nMenus allow h/j/k/l and, when enabled, arrow keys. Gameplay accepts only commands taught so far."
    );
    frame.render_widget(
        Paragraph::new(text).wrap(Wrap { trim: true }).block(
            Block::default()
                .title("Courier Manual")
                .borders(Borders::ALL),
        ),
        chunks[1],
    );
    frame.render_widget(
        Paragraph::new(
            "Press h for Home, l for Lessons, p for Practice, s for Settings, q to quit.",
        )
        .block(Block::default().title("Navigation").borders(Borders::ALL)),
        chunks[2],
    );
}

fn draw_settings(frame: &mut Frame<'_>, app: &App) {
    let chunks = page_layout(frame.area());
    render_title(frame, chunks[0], "Settings", &app.banner);
    let text = format!(
        "c  Color intensity: {}\na  Arrow-key menus: {}\nr  Reset progress\n\nSettings save when you quit normally.",
        if app.progress.color_intense {
            "high"
        } else {
            "low"
        },
        if app.progress.menu_arrows {
            "enabled"
        } else {
            "disabled"
        },
    );
    frame.render_widget(
        Paragraph::new(text).block(Block::default().title("Toggles").borders(Borders::ALL)),
        chunks[1],
    );
    frame.render_widget(
        Paragraph::new("Press h for Home or q to quit.")
            .block(Block::default().borders(Borders::ALL)),
        chunks[2],
    );
}

fn draw_level(frame: &mut Frame<'_>, session: &GameSession, footer: Option<&str>) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(5),
        ])
        .split(area);
    let title = format!("{} {}", session.level.id, session.level.title);
    render_title(frame, chunks[0], &title, &session.message);
    match &session.state {
        PlayState::Grid(grid) => draw_grid(frame, chunks[1], grid),
        PlayState::Token(token) => draw_token(frame, chunks[1], token),
        PlayState::Edit(edit) => draw_edit(frame, chunks[1], edit),
    }
    let budget = match &session.state {
        PlayState::Grid(grid) => grid.command_budget,
        PlayState::Token(token) => token.command_budget,
        PlayState::Edit(edit) => edit.command_budget,
    };
    let mode = match &session.state {
        PlayState::Edit(edit) => match edit.mode {
            EditorMode::Normal => "NORMAL",
            EditorMode::Insert => "INSERT",
            EditorMode::ChangeWord => "CHANGE",
        },
        _ => "NORMAL",
    };
    let hazard = match &session.state {
        PlayState::Grid(grid) => grid.lava_status(session.elapsed()).unwrap_or_default(),
        _ => String::new(),
    };
    let bottom = format!(
        "Mode: {mode} | Command: {} | Used: {}/{}{} | Vocabulary: {}{}",
        if session.input.is_empty() {
            "_".to_string()
        } else {
            session.input.clone()
        },
        session.accepted_commands,
        budget,
        hazard,
        session.commands.help(),
        footer.map_or(String::new(), |text| format!(" | {text}"))
    );
    frame.render_widget(
        Paragraph::new(bottom)
            .wrap(Wrap { trim: true })
            .block(Block::default().title("Input").borders(Borders::ALL)),
        chunks[2],
    );
    if let Some(outcome) = &session.finished {
        let popup = centered_rect(70, 30, frame.area());
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(outcome.summary())
                .wrap(Wrap { trim: true })
                .alignment(Alignment::Center)
                .block(
                    Block::default()
                        .title("Route Complete")
                        .borders(Borders::ALL),
                ),
            popup,
        );
    }
}

fn draw_grid(frame: &mut Frame<'_>, area: Rect, grid: &GridState) {
    let mut lines = Vec::new();
    let inner_width = area.width.saturating_sub(2) as usize;
    let inner_height = area.height.saturating_sub(2) as usize;
    let label_width = 5usize;
    let visible_rows = inner_height.max(1).min(grid.tiles.len());
    let visible_cols = ((inner_width.saturating_sub(label_width)) / 2)
        .max(1)
        .min(grid.tiles.first().map_or(0, Vec::len));
    let y_start = viewport_start(grid.player.y, visible_rows, grid.tiles.len());
    let x_start = viewport_start(
        grid.player.x,
        visible_cols,
        grid.tiles.first().map_or(0, Vec::len),
    );
    let y_end = min(y_start + visible_rows, grid.tiles.len());
    let x_end = min(
        x_start + visible_cols,
        grid.tiles.first().map_or(0, Vec::len),
    );

    for y in y_start..y_end {
        let row = &grid.tiles[y];
        let mut spans = Vec::new();
        spans.push(Span::styled(
            relative_line_label(y, grid.player.y),
            Style::default().fg(Color::DarkGray),
        ));
        for (x, tile) in row.iter().enumerate().take(x_end).skip(x_start) {
            let pos = Pos { x, y };
            let in_lava = grid.lava && y >= grid.tiles.len().saturating_sub(grid.lava_rows);
            let (glyph, color) = if pos == grid.player {
                ("@ ", Color::Yellow)
            } else if pos == grid.goal {
                ("X ", Color::Green)
            } else if in_lava {
                ("~~", Color::Red)
            } else if *tile == Tile::Wall {
                ("##", Color::DarkGray)
            } else {
                (". ", Color::Gray)
            };
            spans.push(Span::styled(glyph, Style::default().fg(color)));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title("Terminal Floor")
                .borders(Borders::ALL),
        ),
        area,
    );
}

fn draw_token(frame: &mut Frame<'_>, area: Rect, token: &TokenState) {
    let targets: HashSet<Pos> = token.targets.iter().copied().collect();
    let mut lines = Vec::new();
    for (y, line) in token.lines.iter().enumerate() {
        let mut spans = Vec::new();
        spans.push(Span::styled(
            relative_line_label(y, token.cursor.y),
            Style::default().fg(Color::DarkGray),
        ));
        for (x, ch) in line.chars().enumerate() {
            let pos = Pos { x, y };
            let mut style = Style::default().fg(Color::Gray);
            if targets.contains(&pos) {
                style = style.fg(Color::Green).add_modifier(Modifier::BOLD);
            }
            if token.cursor == pos {
                style = style.bg(Color::Yellow).fg(Color::Black);
            }
            spans.push(Span::styled(ch.to_string(), style));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().title("Code Rails").borders(Borders::ALL)),
        area,
    );
}

fn draw_edit(frame: &mut Frame<'_>, area: Rect, edit: &EditState) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);
    let mut buffer = Vec::new();
    for (y, line) in edit.lines.iter().enumerate() {
        let mut spans = Vec::new();
        spans.push(Span::styled(
            relative_line_label(y, edit.cursor.y),
            Style::default().fg(Color::DarkGray),
        ));
        for (x, ch) in line.chars().enumerate() {
            let mut style = Style::default().fg(Color::Gray);
            if edit.cursor == (Pos { x, y }) {
                style = style.bg(Color::Yellow).fg(Color::Black);
            }
            spans.push(Span::styled(ch.to_string(), style));
        }
        if line.is_empty() {
            spans.push(Span::raw(" "));
        }
        buffer.push(Line::from(spans));
    }
    let target: Vec<Line> = edit
        .expected
        .iter()
        .enumerate()
        .map(|(y, line)| {
            Line::from(vec![
                Span::styled(
                    format!("{:>3} ", y + 1),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled((*line).clone(), Style::default().fg(Color::Green)),
            ])
        })
        .collect();
    frame.render_widget(
        Paragraph::new(buffer).block(Block::default().title("Buffer").borders(Borders::ALL)),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new(target).block(Block::default().title("Target").borders(Borders::ALL)),
        chunks[1],
    );
}

fn viewport_start(cursor: usize, visible: usize, total: usize) -> usize {
    if visible >= total {
        0
    } else {
        cursor.saturating_sub(visible / 2).min(total - visible)
    }
}

fn relative_line_label(row: usize, cursor_row: usize) -> String {
    if row == cursor_row {
        format!("{:>3} ", row + 1)
    } else {
        format!("{:>3} ", row.abs_diff(cursor_row))
    }
}

fn render_title(frame: &mut Frame<'_>, area: Rect, title: &str, subtitle: &str) {
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                title.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                subtitle.to_string(),
                Style::default().fg(Color::Gray),
            )),
        ])
        .block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn page_layout(area: Rect) -> Vec<Rect> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(10),
            Constraint::Length(5),
        ])
        .split(area)
        .to_vec()
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

fn level_status(level: &Level, progress: &Progress) -> &'static str {
    if !progress.is_unlocked(&level.id) {
        "locked"
    } else if progress.is_cleared(&level.id) {
        "cleared"
    } else {
        "open"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lesson_one_counts_and_motions() {
        assert_eq!(
            parse_command("3j", CommandSet::for_lesson(1)).unwrap(),
            ParsedCommand::Motion(MotionKind::Down, 3)
        );
        assert_eq!(
            parse_command("h", CommandSet::for_lesson(1)).unwrap(),
            ParsedCommand::Motion(MotionKind::Left, 1)
        );
        assert!(parse_command("w", CommandSet::for_lesson(1)).is_err());
    }

    #[test]
    fn parses_word_motions_after_lesson_two() {
        assert_eq!(
            parse_command("ge", CommandSet::for_lesson(2)).unwrap(),
            ParsedCommand::Motion(MotionKind::WordEndBackward, 1)
        );
        assert_eq!(
            parse_command("2w", CommandSet::for_lesson(2)).unwrap(),
            ParsedCommand::Motion(MotionKind::WordForward, 2)
        );
    }

    #[test]
    fn parses_edit_commands_only_after_lesson_three() {
        assert!(parse_command("dw", CommandSet::for_lesson(2)).is_err());
        assert_eq!(
            parse_command("rx", CommandSet::for_lesson(3)).unwrap(),
            ParsedCommand::Replace('x')
        );
        assert_eq!(
            parse_command("cw", CommandSet::for_lesson(3)).unwrap(),
            ParsedCommand::ChangeWord
        );
    }

    #[test]
    fn unlocks_lessons_from_required_intermediate_clears() {
        let mut progress = Progress::new();
        assert!(progress.is_unlocked("1-3"));
        assert!(!progress.is_unlocked("2-0"));
        progress.record_clear("1-2", Duration::from_millis(10), 5);
        assert!(progress.is_unlocked("2-0"));
        assert!(!progress.is_unlocked("3-0"));
        progress.record_clear("2-2", Duration::from_millis(10), 5);
        assert!(progress.is_unlocked("3-0"));
    }

    #[test]
    fn generated_grid_is_seed_reproducible_and_reachable() {
        let catalog = Catalog::new();
        let level = catalog.level("1-2").unwrap();
        let LevelKind::Grid(spec) = &level.kind else {
            panic!("expected grid");
        };
        let first = GridState::from_spec(spec, 99);
        let second = GridState::from_spec(spec, 99);
        assert_eq!(first.tiles, second.tiles);
        assert!(reachable(&first));
        assert!(first.lava);
    }

    #[test]
    fn lesson_one_uses_larger_spaces_and_timed_lava() {
        let catalog = Catalog::new();
        let one_two = catalog.level("1-2").unwrap();
        let one_three = catalog.level("1-3").unwrap();
        let LevelKind::Grid(spec_two) = &one_two.kind else {
            panic!("expected grid");
        };
        let LevelKind::Grid(spec_three) = &one_three.kind else {
            panic!("expected grid");
        };
        assert!(spec_two.width >= 40);
        assert!(spec_two.height >= 20);
        assert!(spec_two.lava_interval_ms.is_some());
        assert!(spec_three.width > spec_two.width);
        assert!(spec_three.height > spec_two.height);
        assert!(spec_three.lava_interval_ms.unwrap() < spec_two.lava_interval_ms.unwrap());
    }

    #[test]
    fn lava_rises_from_elapsed_time_without_commands() {
        let catalog = Catalog::new();
        let level = catalog.level("1-2").unwrap();
        let LevelKind::Grid(spec) = &level.kind else {
            panic!("expected grid");
        };
        let mut grid = GridState::from_spec(spec, 99);
        assert_eq!(grid.lava_rows, 1);
        grid.update_lava(Duration::from_millis(
            spec.lava_interval_ms.unwrap() * 2 + 10,
        ));
        assert_eq!(grid.lava_rows, 3);
    }

    #[test]
    fn relative_line_labels_match_vim_style() {
        assert_eq!(relative_line_label(4, 4), "  5 ");
        assert_eq!(relative_line_label(3, 4), "  1 ");
        assert_eq!(relative_line_label(1, 4), "  3 ");
        assert_eq!(relative_line_label(7, 4), "  3 ");
    }

    #[test]
    fn token_targets_are_valid() {
        let catalog = Catalog::new();
        for level in catalog.all_levels() {
            if let LevelKind::Token(spec) = &level.kind {
                let token = TokenState::from_spec(spec, 11);
                for target in token.targets {
                    assert!(target.y < token.lines.len());
                    assert!(target.x < token.lines[target.y].chars().count());
                }
            }
        }
    }

    #[test]
    fn edit_snippets_have_expected_output() {
        let catalog = Catalog::new();
        for level in catalog.all_levels() {
            if let LevelKind::Edit(spec) = &level.kind {
                let edit = EditState::from_spec(spec);
                assert_ne!(edit.lines, edit.expected);
                assert!(!edit.expected.is_empty());
            }
        }
    }

    #[test]
    fn hints_name_better_commands() {
        let moves = vec![
            MotionKind::Right,
            MotionKind::Right,
            MotionKind::Right,
            MotionKind::Right,
        ];
        assert!(hint_for_counts(&moves).unwrap().contains("4l"));
        let history = vec!["l".to_string(); 6];
        assert!(hint_for_words(&history).unwrap().contains("w"));
    }

    fn reachable(grid: &GridState) -> bool {
        let mut seen = HashSet::new();
        let mut stack = vec![grid.player];
        while let Some(pos) = stack.pop() {
            if !seen.insert(pos) {
                continue;
            }
            if pos == grid.goal {
                return true;
            }
            for next in [
                Pos {
                    x: pos.x.saturating_sub(1),
                    y: pos.y,
                },
                Pos {
                    x: pos.x + 1,
                    y: pos.y,
                },
                Pos {
                    x: pos.x,
                    y: pos.y.saturating_sub(1),
                },
                Pos {
                    x: pos.x,
                    y: pos.y + 1,
                },
            ] {
                if grid.is_floor(next) {
                    stack.push(next);
                }
            }
        }
        false
    }
}
