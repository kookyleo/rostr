//! Interactive TUI for `rostr discover`. All state changes go through `Selection`.

use std::io;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};

use crate::discover::{AgentReport, source_details};
use crate::model_id;
use crate::selection::{Changes, Selection, Status};

pub enum Outcome {
    Save,
    Cancel,
    /// Enter with nothing to save: no edits, no new models, scope.yaml exists.
    NoChanges,
}

/// Lines per change category on the confirmation screen.
const CONFIRM_LIST_MAX: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Group(usize),
    Item(usize, usize),
    /// A blank line before the patterns. The cursor never stops on it.
    Spacer,
    PatternsHeader,
    Pattern(usize),
}

struct App<'a> {
    sel: &'a mut Selection,
    /// The state at the start of the session, to show what a save changes.
    before: Selection,
    scope_exists: bool,
    agents: &'a [AgentReport],
    /// The save confirmation screen is open.
    confirm: bool,
    /// Esc was pressed once with unsaved edits; a second Esc discards them.
    discard: bool,
    /// The key reference is open.
    help: bool,
    cursor: usize,
    list: ListState,
    input: Option<String>,
    message: Option<(String, bool)>,
}

pub fn run(sel: &mut Selection, agents: &[AgentReport], scope_exists: bool) -> io::Result<Outcome> {
    let mut terminal = ratatui::init();
    let mut app = App {
        before: sel.clone(),
        sel,
        scope_exists,
        agents,
        confirm: false,
        discard: false,
        help: false,
        cursor: 0,
        list: ListState::default(),
        input: None,
        message: None,
    };
    let result = app.event_loop(&mut terminal);
    ratatui::restore();
    result
}

impl App<'_> {
    fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for (g, group) in self.sel.groups.iter().enumerate() {
            rows.push(Row::Group(g));
            if group.expanded {
                rows.extend((0..group.items.len()).map(|i| Row::Item(g, i)));
            }
        }
        if !rows.is_empty() {
            rows.push(Row::Spacer);
        }
        rows.push(Row::PatternsHeader);
        rows.extend((0..self.sel.patterns.len()).map(Row::Pattern));
        rows
    }

    fn current(&self) -> Option<Row> {
        self.rows().get(self.cursor).copied()
    }

    fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> io::Result<Outcome> {
        loop {
            terminal.draw(|f| self.draw(f))?;
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                return Ok(Outcome::Cancel);
            }
            if self.help {
                self.help = false;
                continue;
            }
            if self.confirm {
                match key.code {
                    KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                        return Ok(Outcome::Save);
                    }
                    KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') => {
                        self.confirm = false;
                    }
                    _ => {}
                }
                continue;
            }
            if self.input.is_some() {
                self.input_key(key);
                continue;
            }
            if let Some(outcome) = self.normal_key(key) {
                return Ok(outcome);
            }
        }
    }

    fn input_key(&mut self, key: KeyEvent) {
        let Some(buf) = self.input.as_mut() else {
            return;
        };
        if matches!(
            key.code,
            KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Esc
        ) {
            self.message = None;
        }
        match key.code {
            KeyCode::Esc => self.input = None,
            KeyCode::Backspace => {
                buf.pop();
            }
            KeyCode::Char(c) => buf.push(c),
            KeyCode::Enter => {
                let text = buf.trim().to_string();
                if text.is_empty() {
                    self.input = None;
                    return;
                }
                match self.sel.add(&text) {
                    Ok(msg) => {
                        self.input = None;
                        self.message = Some((msg, false));
                        self.focus_on(&model_id::normalize(&text));
                    }
                    Err(e) => self.message = Some((e.to_string(), true)),
                }
            }
            _ => {}
        }
    }

    fn focus_on(&mut self, text: &str) {
        let rows = self.rows();
        let hit = rows.iter().position(|r| match *r {
            Row::Item(g, i) => self.sel.groups[g].items[i].id == text,
            Row::Pattern(p) => self.sel.patterns[p] == text,
            _ => false,
        });
        if let Some(pos) = hit {
            self.cursor = pos;
        }
    }

    fn normal_key(&mut self, key: KeyEvent) -> Option<Outcome> {
        let len = self.rows().len();
        let before = self.cursor;
        let discard = std::mem::take(&mut self.discard);
        self.message = None;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if discard || self.changes().is_empty() {
                    return Some(Outcome::Cancel);
                }
                self.discard = true;
            }
            KeyCode::Char('?') => self.help = true,
            KeyCode::Enter => {
                if !self.needs_save() {
                    return Some(Outcome::NoChanges);
                }
                self.confirm = true;
            }
            KeyCode::Up | KeyCode::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.cursor = (self.cursor + 1).min(len - 1),
            KeyCode::Home | KeyCode::Char('g') => self.cursor = 0,
            KeyCode::End | KeyCode::Char('G') => self.cursor = len - 1,
            KeyCode::PageUp => self.cursor = self.cursor.saturating_sub(10),
            KeyCode::PageDown => self.cursor = (self.cursor + 10).min(len - 1),
            KeyCode::Char(' ') => match self.current() {
                Some(Row::Group(g)) => {
                    let msg = self.sel.toggle_group(g);
                    self.message = Some((msg, false));
                }
                Some(Row::Item(g, i)) => self.sel.toggle(g, i),
                _ => {}
            },
            KeyCode::Char('a') | KeyCode::Char('A') => self.sel.toggle_all(),
            KeyCode::Left | KeyCode::Char('h') => match self.current() {
                Some(Row::Group(g)) => self.sel.groups[g].expanded = false,
                Some(Row::Item(g, _)) => {
                    self.sel.groups[g].expanded = false;
                    self.cursor = self
                        .rows()
                        .iter()
                        .position(|r| *r == Row::Group(g))
                        .unwrap_or(0);
                }
                _ => {}
            },
            KeyCode::Right | KeyCode::Char('l') => {
                if let Some(Row::Group(g)) = self.current() {
                    self.sel.groups[g].expanded = true;
                }
            }
            KeyCode::Char('+') | KeyCode::Char('i') => self.input = Some(String::new()),
            KeyCode::Char('d') | KeyCode::Char('D') | KeyCode::Delete => match self.current() {
                Some(Row::Item(g, i)) => {
                    if let Err(e) = self.sel.toggle_remove(g, i) {
                        self.message = Some((e.to_string(), true));
                    }
                }
                Some(Row::Pattern(p)) => {
                    let pattern = self.sel.remove_pattern(p);
                    self.message = Some((format!("removed pattern `{pattern}`"), false));
                }
                _ => {}
            },
            _ => {}
        }
        self.cursor = self.cursor.min(self.rows().len() - 1);
        // Step over the spacer in the direction of the move. It is never the
        // first or the last row, so both neighbours exist.
        if self.current() == Some(Row::Spacer) {
            self.cursor = if self.cursor < before {
                self.cursor - 1
            } else {
                self.cursor + 1
            };
        }
        None
    }

    fn draw(&mut self, f: &mut Frame) {
        let [header, body, detail, status, footer] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(2),
        ])
        .areas(f.area());

        f.render_widget(Paragraph::new(self.header_lines()), header);

        if self.help {
            f.render_widget(Paragraph::new(help_lines()), body);
            let help = vec![Line::from("Any key Close"), Line::default()];
            f.render_widget(Paragraph::new(help).dim(), footer);
            return;
        }
        if self.confirm {
            f.render_widget(Paragraph::new(self.confirm_lines()), body);
            let help = vec![Line::from("Enter/y Save   Esc/n Back"), Line::default()];
            f.render_widget(Paragraph::new(help).dim(), footer);
            return;
        }

        let width = body.width as usize;
        let rows = self.rows();
        let items: Vec<ListItem> = rows
            .iter()
            .map(|r| ListItem::new(self.row_line(*r, width)))
            .collect();
        self.list.select(Some(self.cursor));
        let list = List::new(items).highlight_style(Style::new().add_modifier(Modifier::REVERSED));
        f.render_stateful_widget(list, body, &mut self.list);

        f.render_widget(Paragraph::new(self.detail_line()).dim(), detail);

        let status_line = match (&self.input, &self.message) {
            (None, _) if self.discard => Line::from(format!(
                "Discard {} unsaved changes? Press Esc again to discard, any other key to go on.",
                self.changes().len()
            ))
            .fg(Color::Yellow),
            (Some(buf), msg) => Line::from(vec![
                Span::raw("Add model ID or pattern: ").bold(),
                Span::raw(buf.as_str()),
                Span::raw("▏"),
                match msg {
                    Some((err, true)) => Span::raw(format!("   {err}")).fg(Color::Red),
                    _ => Span::raw("   (e.g. openai/gpt-5.3-codex, xai/grok-*)").dim(),
                },
            ]),
            (None, Some((msg, true))) => Line::from(msg.as_str()).fg(Color::Red),
            (None, Some((msg, false))) => Line::from(msg.as_str()).fg(Color::Green),
            (None, None) => Line::default(),
        };
        f.render_widget(Paragraph::new(status_line), status);

        let help = if self.input.is_some() {
            vec![Line::from("Enter Add   Esc Back"), Line::default()]
        } else {
            vec![
                Line::from(
                    "↑↓/jk Navigate  Space Toggle  A Toggle All  ←→/hl Fold  + Add  D Remove",
                ),
                Line::from("Enter Save   Esc/q Cancel   ? All keys"),
            ]
        };
        f.render_widget(Paragraph::new(help).dim(), footer);
    }

    fn header_lines(&self) -> Vec<Line<'static>> {
        let installed: Vec<String> = self
            .agents
            .iter()
            .filter(|a| a.installed)
            .map(|a| {
                if a.signed_in {
                    format!("{} (signed in)", a.name)
                } else {
                    a.name.to_string()
                }
            })
            .collect();
        let agents = if installed.is_empty() {
            "none found".to_string()
        } else {
            installed.join(", ")
        };
        let (selected, total) = self.sel.counts();
        let warnings: usize = self.agents.iter().map(|a| a.warnings.len()).sum();
        let mut summary = vec![
            Span::raw(format!("{selected}/{total} selected · ")),
            Span::raw(format!("{} new", self.sel.count(Status::New))).fg(Color::Green),
            Span::raw(format!(" · {} missing", self.sel.count(Status::Missing))),
        ];
        if warnings > 0 {
            summary.push(
                Span::raw(format!(
                    " · {warnings} warnings (see `rostr discover --dry-run`)"
                ))
                .fg(Color::Yellow),
            );
        }
        vec![
            Line::from("Rostr Discover").bold(),
            Line::from(format!("Agents: {agents}")).dim(),
            Line::from(summary).dim(),
        ]
    }

    fn row_line(&self, row: Row, width: usize) -> Line<'static> {
        match row {
            Row::Group(g) => {
                let group = &self.sel.groups[g];
                let arrow = if group.expanded { "▼" } else { "▶" };
                let left = format!("{arrow} {}", model_id::provider_display(&group.provider));
                let (selected, listed) = self.sel.group_counts(g);
                let right = format!("{selected}/{listed}");
                let pad = width
                    .saturating_sub(left.chars().count() + right.len() + 1)
                    .max(1);
                Line::from(format!("{left}{}{right}", " ".repeat(pad))).bold()
            }
            Row::Item(g, i) => {
                let item = &self.sel.groups[g].items[i];
                let mark = if item.remove {
                    "[-]"
                } else if self.sel.is_selected(item) {
                    "[x]"
                } else {
                    "[ ]"
                };
                let name_w = self.name_width();
                let name: String = item.name.chars().take(name_w).collect();
                // New rows are green and missing rows are dim, in addition to the tag.
                let row_style = match item.status {
                    Status::New => Some(Style::new().fg(Color::Green)),
                    Status::Missing => Some(Style::new().add_modifier(Modifier::DIM)),
                    Status::Known | Status::Manual => None,
                };
                let sources = Span::raw(item.source_label());
                let sources = if row_style.is_some() {
                    sources
                } else {
                    sources.fg(Color::Blue)
                };
                let mut spans = vec![
                    Span::raw(format!("  {mark} ")),
                    Span::raw(format!("{name:<name_w$}  ")),
                    sources,
                ];
                let tag = match item.status {
                    Status::New => Some("  new"),
                    Status::Missing => Some("  missing"),
                    Status::Known | Status::Manual => None,
                };
                spans.extend(tag.map(Span::raw));
                if item.remove {
                    spans.push(Span::raw("  will remove").fg(Color::Red));
                }
                let mut line = Line::from(spans);
                if let Some(style) = row_style {
                    line = line.style(style);
                }
                if item.remove {
                    line.crossed_out()
                } else {
                    line
                }
            }
            Row::Spacer => Line::default(),
            Row::PatternsHeader => {
                let hint = if self.sel.patterns.is_empty() {
                    "  none: only exceptions are selected"
                } else {
                    "  a model is selected when a pattern matches, unless an exception says otherwise"
                };
                Line::from(vec![Span::raw("Patterns").bold(), Span::raw(hint).dim()])
            }
            Row::Pattern(p) => {
                let pattern = &self.sel.patterns[p];
                let n = self
                    .sel
                    .items()
                    .filter(|i| !i.remove && model_id::wildcard_match(pattern, &i.id))
                    .count();
                Line::from(vec![
                    Span::raw(format!("  {pattern}")),
                    Span::raw(format!("  {n} match")).dim(),
                ])
            }
        }
    }

    fn changes(&self) -> Changes {
        self.sel.changes(&self.before)
    }

    /// A save writes something: an edit, new models for the database, or a
    /// first scope.yaml.
    fn needs_save(&self) -> bool {
        !self.scope_exists || self.sel.count(Status::New) > 0 || !self.changes().is_empty()
    }

    fn confirm_lines(&self) -> Vec<Line<'static>> {
        let c = self.changes();
        let scope_state = if !self.scope_exists {
            "will be created"
        } else if self.before.to_scope() != self.sel.to_scope() {
            "will be updated"
        } else {
            "unchanged"
        };
        let mut lines = vec![
            Line::from("Save these changes?").bold(),
            Line::default(),
            Line::from(vec![
                Span::raw("scope.yaml").bold(),
                Span::raw(format!("  {scope_state}")).dim(),
            ]),
        ];
        let mut list = |label: &'static str, ids: &[String], color: Color| {
            for id in ids.iter().take(CONFIRM_LIST_MAX) {
                lines.push(Line::from(vec![
                    Span::raw(format!("  {label:<14}")).fg(color),
                    Span::raw(id.clone()),
                ]));
            }
            if ids.len() > CONFIRM_LIST_MAX {
                lines.push(
                    Line::from(format!("  … and {} more", ids.len() - CONFIRM_LIST_MAX)).dim(),
                );
            }
        };
        list("+ pattern", &c.patterns_added, Color::Green);
        list("- pattern", &c.patterns_removed, Color::Red);
        list("+ select", &c.selected, Color::Green);
        list("- select", &c.unselected, Color::Yellow);
        list("+ add", &c.added, Color::Green);
        list("- remove", &c.removed, Color::Red);
        if let Some((old, new)) = c.exceptions {
            lines.push(Line::from(format!("  exceptions    {old} → {new}")).dim());
        }
        if c.is_empty() && !self.scope_exists {
            lines.push(
                Line::from(format!(
                    "  {} patterns, no exceptions",
                    self.sel.patterns.len()
                ))
                .dim(),
            );
        }
        let (selected, total) = self.sel.counts();
        lines.push(Line::default());
        lines.push(Line::from(vec![
            Span::raw("rostr.db").bold(),
            Span::raw(format!(
                "  record {total} models ({selected} selected, {} new)",
                self.sel.count(Status::New)
            ))
            .dim(),
        ]));
        if !c.removed.is_empty() {
            lines.push(
                Line::from(format!(
                    "  delete {} models; this cannot be undone",
                    c.removed.len()
                ))
                .fg(Color::Red),
            );
        }
        lines
    }

    fn name_width(&self) -> usize {
        let widest = self.sel.items().map(|i| i.name.chars().count()).max();
        widest.unwrap_or(0).clamp(12, 32)
    }

    fn detail_line(&self) -> Line<'static> {
        match self.current() {
            Some(Row::Item(g, i)) => {
                let item = &self.sel.groups[g].items[i];
                let from = if item.sources.is_empty() {
                    "no recorded source".to_string()
                } else {
                    source_details(&item.sources)
                };
                let reason = self.sel.resolve(&item.id).1.describe();
                Line::from(format!("{}  ·  by {reason}  ·  {from}", item.id))
            }
            Some(Row::Group(g)) => Line::from(format!(
                "Space sets or removes the {}/* pattern",
                self.sel.groups[g].provider
            )),
            Some(Row::Pattern(_)) => Line::from("D removes this pattern"),
            Some(Row::PatternsHeader) => {
                Line::from("Press + and type a pattern such as anthropic/* to add one")
            }
            Some(Row::Spacer) | None => Line::default(),
        }
    }
}

/// The full key reference, opened with `?`.
fn help_lines() -> Vec<Line<'static>> {
    const SECTIONS: &[(&str, &[(&str, &str)])] = &[
        (
            "Navigate",
            &[
                ("↑ k  ↓ j", "previous / next row"),
                ("PgUp PgDn", "10 rows up / down"),
                ("Home g  End G", "first / last row"),
                ("→ l", "expand a provider"),
                ("← h", "fold a provider (also from one of its models)"),
            ],
        ),
        (
            "Select",
            &[
                (
                    "Space",
                    "toggle a model; on a provider, set or remove <provider>/*",
                ),
                ("a A", "toggle all models"),
            ],
        ),
        (
            "Edit",
            &[
                ("+ i", "add a model ID or a wildcard pattern"),
                (
                    "d D Delete",
                    "mark a missing model for removal (again to undo)",
                ),
                ("", "on a model you added, or on a pattern: remove it"),
            ],
        ),
        (
            "Save and exit",
            &[
                (
                    "Enter",
                    "review the changes; then Enter or y saves, Esc or n goes back",
                ),
                (
                    "Esc q",
                    "exit; with unsaved edits, press Esc again to discard them",
                ),
                ("Ctrl-C", "exit at once, save nothing"),
                ("?", "this reference"),
            ],
        ),
    ];
    let mut lines = vec![Line::from("Keys").bold()];
    for (title, keys) in SECTIONS {
        lines.push(Line::default());
        lines.push(Line::from(*title).bold());
        for (key, what) in *keys {
            lines.push(Line::from(vec![
                Span::raw(format!("  {key:<16}")).fg(Color::Cyan),
                Span::raw(*what),
            ]));
        }
    }
    lines.push(Line::default());
    lines.push(Line::from("Marks").bold());
    lines.push(Line::from(vec![
        Span::raw("  [x] selected   [ ] not selected   [-] will be removed   "),
        Span::raw("green").fg(Color::Green),
        Span::raw(" new   "),
        Span::raw("dim").dim(),
        Span::raw(" missing"),
    ]));
    lines
}
