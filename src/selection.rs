//! Merges discovered candidates, known models and the scope into the editable
//! state behind the TUI. Contains no terminal code.
//!
//! A model's state is never stored per model: it is computed from `patterns`
//! and `exceptions`. A toggle that agrees with the patterns drops the
//! exception; a toggle that disagrees writes one.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};

use crate::discover::{Candidate, Source, agent_list};
use crate::model_id;
use crate::scope::{self, Exception, Reason, Scope, VERSION};
use crate::store::{Known, Resolved};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Found in this scan for the first time.
    New,
    /// Found in this scan and known from earlier scans.
    Known,
    /// Known from earlier scans or `scope.yaml`, but not found in this scan.
    Missing,
    /// Added by hand in this session.
    Manual,
}

#[derive(Debug, Clone)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub status: Status,
    /// Sources from this scan, or the last recorded sources for a missing model.
    pub sources: Vec<Source>,
    /// The user asked to drop this model from the scope and the database on save.
    pub remove: bool,
}

impl Item {
    /// The source column of the listing: `(Claude Code, OpenCode)`. A missing
    /// model shows its last known agents; its status tag says it is missing.
    pub fn source_label(&self) -> String {
        match self.status {
            Status::Manual => "(added by hand)".into(),
            _ if self.sources.is_empty() => "(scope.yaml)".into(),
            _ => format!("({})", agent_list(&self.sources)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Group {
    pub provider: String,
    pub expanded: bool,
    pub items: Vec<Item>,
}

/// What a save changes, compared with the state at the start of the session.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub patterns_added: Vec<String>,
    pub patterns_removed: Vec<String>,
    /// Exception count before and after, when the exceptions differ.
    pub exceptions: Option<(usize, usize)>,
    /// Listed models that become selected or unselected.
    pub selected: Vec<String>,
    pub unselected: Vec<String>,
    /// Models added by hand.
    pub added: Vec<String>,
    /// Models dropped from the scope and the database.
    pub removed: Vec<String>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self == &Changes::default()
    }

    /// Number of edits, for the discard prompt. An exception rewrite with no
    /// other visible effect counts as one edit.
    pub fn len(&self) -> usize {
        let n = self.patterns_added.len()
            + self.patterns_removed.len()
            + self.selected.len()
            + self.unselected.len()
            + self.added.len()
            + self.removed.len();
        if n == 0 && self.exceptions.is_some() {
            1
        } else {
            n
        }
    }
}

#[derive(Debug, Clone)]
pub struct Selection {
    pub groups: Vec<Group>,
    pub patterns: Vec<String>,
    exceptions: BTreeMap<String, bool>,
}

impl Selection {
    /// `scope` is `None` on the first run: then every discovered provider gets
    /// a `<provider>/*` pattern, so all new models start selected.
    pub fn merge(scope: Option<&Scope>, known: &[Known], candidates: &[Candidate]) -> Selection {
        let (patterns, exceptions) = match scope {
            Some(s) => (
                s.patterns.clone(),
                s.exceptions
                    .iter()
                    .map(|e| (e.id.clone(), e.selected))
                    .collect(),
            ),
            None => {
                let providers: BTreeSet<&str> = candidates
                    .iter()
                    .map(|c| model_id::provider_of(&c.id))
                    .collect();
                let patterns = providers.into_iter().map(|p| format!("{p}/*")).collect();
                (patterns, BTreeMap::new())
            }
        };
        let known_ids: BTreeSet<&str> = known.iter().map(|k| k.id.as_str()).collect();
        let mut items: BTreeMap<String, Item> = BTreeMap::new();
        for c in candidates {
            let status = if known_ids.contains(c.id.as_str()) || exceptions.contains_key(&c.id) {
                Status::Known
            } else {
                Status::New
            };
            items.insert(
                c.id.clone(),
                Item {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    status,
                    sources: c.sources.clone(),
                    remove: false,
                },
            );
        }
        for k in known {
            items.entry(k.id.clone()).or_insert_with(|| Item {
                id: k.id.clone(),
                name: k.name.clone(),
                status: Status::Missing,
                sources: k.sources.clone(),
                remove: false,
            });
        }
        for id in exceptions.keys() {
            items.entry(id.clone()).or_insert_with(|| Item {
                id: id.clone(),
                name: model_id::display_name(id),
                status: Status::Missing,
                sources: Vec::new(),
                remove: false,
            });
        }
        let mut sel = Selection {
            groups: Vec::new(),
            patterns,
            exceptions,
        };
        for item in items.into_values() {
            sel.insert(item);
        }
        sel
    }

    fn insert(&mut self, item: Item) {
        let provider = model_id::provider_of(&item.id).to_string();
        let gi = match self.groups.iter().position(|g| g.provider == provider) {
            Some(gi) => gi,
            None => {
                let at = self.groups.partition_point(|g| g.provider < provider);
                self.groups.insert(
                    at,
                    Group {
                        provider,
                        expanded: true,
                        items: Vec::new(),
                    },
                );
                at
            }
        };
        let items = &mut self.groups[gi].items;
        let at = items.partition_point(|i| i.id < item.id);
        items.insert(at, item);
    }

    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.groups.iter().flat_map(|g| g.items.iter())
    }

    fn live(&self) -> impl Iterator<Item = &Item> {
        self.items().filter(|i| !i.remove)
    }

    pub fn resolve(&self, id: &str) -> (bool, Reason) {
        match self.exceptions.get(id) {
            Some(&selected) => (selected, Reason::Exception),
            None => scope::resolve_patterns(&self.patterns, id),
        }
    }

    pub fn is_selected(&self, item: &Item) -> bool {
        !item.remove && self.resolve(&item.id).0
    }

    /// `(selected, listed)` counts for one group, without models marked for removal.
    pub fn group_counts(&self, g: usize) -> (usize, usize) {
        let items = self.groups[g].items.iter().filter(|i| !i.remove);
        items.fold((0, 0), |(s, n), i| {
            (s + self.is_selected(i) as usize, n + 1)
        })
    }

    pub fn counts(&self) -> (usize, usize) {
        self.live().fold((0, 0), |(s, n), i| {
            (s + self.is_selected(i) as usize, n + 1)
        })
    }

    pub fn count(&self, status: Status) -> usize {
        self.live().filter(|i| i.status == status).count()
    }

    /// Records a choice for one model with the fewest exceptions.
    fn choose(&mut self, id: &str, selected: bool) {
        if scope::resolve_patterns(&self.patterns, id).0 == selected {
            self.exceptions.remove(id);
        } else {
            self.exceptions.insert(id.to_string(), selected);
        }
    }

    pub fn toggle(&mut self, g: usize, i: usize) {
        let item = &mut self.groups[g].items[i];
        item.remove = false;
        let id = item.id.clone();
        let now = self.resolve(&id).0;
        self.choose(&id, !now);
    }

    /// Selects or clears a whole provider through its patterns. Selecting sets
    /// `<provider>/*`; clearing removes every pattern of that provider. Both
    /// drop the provider's exceptions, so the group becomes uniform.
    pub fn toggle_group(&mut self, g: usize) -> String {
        let (selected, listed) = self.group_counts(g);
        self.set_group(g, selected < listed)
    }

    fn set_group(&mut self, g: usize, select: bool) -> String {
        let provider = self.groups[g].provider.clone();
        let prefix = format!("{provider}/");
        self.patterns.retain(|p| !p.starts_with(&prefix));
        self.exceptions.retain(|id, _| !id.starts_with(&prefix));
        for item in &mut self.groups[g].items {
            item.remove = false;
        }
        if select {
            self.patterns.push(format!("{provider}/*"));
            format!("pattern `{provider}/*` set")
        } else {
            format!("patterns for `{provider}` removed")
        }
    }

    pub fn toggle_all(&mut self) {
        let (selected, listed) = self.counts();
        for g in 0..self.groups.len() {
            self.set_group(g, selected < listed);
        }
    }

    /// Marks or unmarks a model for removal. Only models not found in this
    /// scan can go: a discovered model stays listed and can only be cleared.
    pub fn toggle_remove(&mut self, g: usize, i: usize) -> Result<()> {
        let item = &mut self.groups[g].items[i];
        match item.status {
            Status::Missing => item.remove = !item.remove,
            Status::Manual => {
                let id = self.groups[g].items.remove(i).id;
                self.exceptions.remove(&id);
                if self.groups[g].items.is_empty() {
                    self.groups.remove(g);
                }
            }
            _ => bail!(
                "`{}` was found in this scan; press Space to exclude it instead",
                item.id
            ),
        }
        Ok(())
    }

    /// Adds a pattern when the input contains `*` or `?`, else a model ID.
    /// Returns a short message for the status line.
    pub fn add(&mut self, input: &str) -> Result<String> {
        let text = model_id::normalize(input);
        if model_id::is_pattern(&text) {
            model_id::validate_pattern(&text)?;
            if self.patterns.contains(&text) {
                bail!("pattern `{text}` already exists");
            }
            self.patterns.push(text.clone());
            let n = self
                .live()
                .filter(|i| model_id::wildcard_match(&text, &i.id))
                .count();
            return Ok(format!("added pattern `{text}` ({n} listed models match)"));
        }
        model_id::validate_id(&text)?;
        if let Some(item) = self
            .groups
            .iter_mut()
            .flat_map(|g| g.items.iter_mut())
            .find(|i| i.id == text)
        {
            item.remove = false;
            self.choose(&text, true);
            return Ok(format!("`{text}` is already listed; selected it"));
        }
        self.insert(Item {
            name: model_id::display_name(&text),
            id: text.clone(),
            status: Status::Manual,
            sources: Vec::new(),
            remove: false,
        });
        // A model added by hand is always pinned, even when a pattern matches it.
        self.exceptions.insert(text.clone(), true);
        Ok(format!("added `{text}`"))
    }

    pub fn remove_pattern(&mut self, p: usize) -> String {
        self.patterns.remove(p)
    }

    fn removed(&self) -> BTreeSet<&str> {
        self.items()
            .filter(|i| i.remove)
            .map(|i| i.id.as_str())
            .collect()
    }

    pub fn to_scope(&self) -> Scope {
        let removed = self.removed();
        Scope {
            version: VERSION,
            patterns: self.patterns.clone(),
            exceptions: self
                .exceptions
                .iter()
                .filter(|(id, _)| !removed.contains(id.as_str()))
                .map(|(id, &selected)| Exception {
                    id: id.clone(),
                    selected,
                })
                .collect(),
        }
        .normalized()
    }

    /// The complete resolved model list for the database.
    pub fn resolved(&self) -> Vec<Resolved> {
        self.live()
            .map(|i| {
                let (selected, reason) = self.resolve(&i.id);
                Resolved {
                    id: i.id.clone(),
                    name: i.name.clone(),
                    selected,
                    reason: reason.describe(),
                }
            })
            .collect()
    }

    pub fn removed_ids(&self) -> Vec<String> {
        self.removed().into_iter().map(String::from).collect()
    }

    /// The edits made since `before`, a copy taken at the start of the session.
    pub fn changes(&self, before: &Selection) -> Changes {
        let (old, new) = (before.to_scope(), self.to_scope());
        let mut changes = Changes {
            patterns_added: diff(&new.patterns, &old.patterns),
            patterns_removed: diff(&old.patterns, &new.patterns),
            exceptions: (old.exceptions != new.exceptions)
                .then_some((old.exceptions.len(), new.exceptions.len())),
            removed: self.removed_ids(),
            ..Changes::default()
        };
        for item in self.live() {
            if item.status == Status::Manual {
                changes.added.push(item.id.clone());
                continue;
            }
            match (before.resolve(&item.id).0, self.resolve(&item.id).0) {
                (false, true) => changes.selected.push(item.id.clone()),
                (true, false) => changes.unselected.push(item.id.clone()),
                _ => {}
            }
        }
        changes
    }
}

fn diff(a: &[String], b: &[String]) -> Vec<String> {
    a.iter().filter(|x| !b.contains(x)).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: &str) -> Candidate {
        Candidate {
            id: id.into(),
            name: model_id::display_name(id),
            sources: vec![Source::new("T", "x")],
        }
    }

    fn known(id: &str) -> Known {
        Known {
            id: id.into(),
            name: model_id::display_name(id),
            sources: vec![Source::new("Old", "x")],
        }
    }

    fn scope(patterns: &[&str], exceptions: &[(&str, bool)]) -> Scope {
        Scope {
            version: 1,
            patterns: patterns.iter().map(|s| s.to_string()).collect(),
            exceptions: exceptions
                .iter()
                .map(|(id, s)| Exception {
                    id: id.to_string(),
                    selected: *s,
                })
                .collect(),
        }
    }

    fn state(sel: &Selection, id: &str) -> (bool, Status) {
        let i = sel.items().find(|i| i.id == id).unwrap();
        (sel.is_selected(i), i.status)
    }

    fn pos(sel: &Selection, id: &str) -> (usize, usize) {
        for (g, group) in sel.groups.iter().enumerate() {
            if let Some(i) = group.items.iter().position(|i| i.id == id) {
                return (g, i);
            }
        }
        panic!("{id} not listed");
    }

    #[test]
    fn first_run_generates_provider_patterns() {
        let sel = Selection::merge(None, &[], &[cand("openai/a"), cand("anthropic/b")]);
        assert_eq!(sel.patterns, vec!["anthropic/*", "openai/*"]);
        assert!(
            sel.items()
                .all(|i| sel.is_selected(i) && i.status == Status::New)
        );
        assert!(sel.to_scope().exceptions.is_empty());
    }

    #[test]
    fn rerun_keeps_exceptions_and_flags_missing() {
        let s = scope(&["anthropic/*"], &[("anthropic/old", false)]);
        let k = [known("anthropic/old"), known("anthropic/gone")];
        let sel = Selection::merge(
            Some(&s),
            &k,
            &[
                cand("anthropic/old"),
                cand("anthropic/new"),
                cand("openai/x"),
            ],
        );
        assert_eq!(state(&sel, "anthropic/old"), (false, Status::Known));
        assert_eq!(state(&sel, "anthropic/new"), (true, Status::New));
        assert_eq!(state(&sel, "openai/x"), (false, Status::New));
        assert_eq!(state(&sel, "anthropic/gone"), (true, Status::Missing));
        // The missing model keeps its last recorded source.
        let gone = sel.items().find(|i| i.id == "anthropic/gone").unwrap();
        assert_eq!(gone.sources[0].agent, "Old");
        assert_eq!(sel.to_scope(), s);
        assert_eq!(sel.resolved().len(), 4);
    }

    #[test]
    fn toggles_write_minimal_exceptions() {
        let mut sel = Selection::merge(
            Some(&scope(&["a/*"], &[])),
            &[],
            &[cand("a/1"), cand("b/1")],
        );
        let (g, i) = pos(&sel, "a/1");
        sel.toggle(g, i);
        assert_eq!(
            sel.to_scope().exceptions,
            vec![Exception {
                id: "a/1".into(),
                selected: false
            }]
        );
        sel.toggle(g, i);
        assert!(
            sel.to_scope().exceptions.is_empty(),
            "back to the pattern default"
        );
        let (g, i) = pos(&sel, "b/1");
        sel.toggle(g, i);
        assert_eq!(
            sel.to_scope().exceptions,
            vec![Exception {
                id: "b/1".into(),
                selected: true
            }]
        );
    }

    #[test]
    fn group_toggle_uses_patterns() {
        let s = scope(&["a/*codex*"], &[("a/codex-old", false), ("a/chat", true)]);
        let c = [
            cand("a/codex-new"),
            cand("a/codex-old"),
            cand("a/chat"),
            cand("b/1"),
        ];
        let mut sel = Selection::merge(Some(&s), &[], &c);
        sel.toggle_group(0);
        assert_eq!(sel.group_counts(0), (3, 3));
        assert_eq!(sel.to_scope().patterns, vec!["a/*"]);
        assert!(sel.to_scope().exceptions.is_empty());
        sel.toggle_group(0);
        assert_eq!(sel.group_counts(0), (0, 3));
        assert!(sel.to_scope().patterns.is_empty());
        sel.toggle_all();
        assert_eq!(sel.counts(), (4, 4));
        assert_eq!(sel.to_scope().patterns, vec!["a/*", "b/*"]);
        sel.toggle_all();
        assert_eq!(sel.counts(), (0, 4));
    }

    #[test]
    fn running_twice_is_stable() {
        let c = [cand("anthropic/a"), cand("openai/b")];
        let first = Selection::merge(None, &[], &c).to_scope();
        let k: Vec<Known> = c.iter().map(|c| known(&c.id)).collect();
        let second = Selection::merge(Some(&first), &k, &c);
        assert_eq!(second.to_scope(), first);
        assert!(second.items().all(|i| i.status == Status::Known));
    }

    #[test]
    fn manual_add_and_patterns() {
        let mut sel = Selection::merge(
            Some(&scope(&[], &[])),
            &[],
            &[cand("openai/gpt-codex"), cand("openai/gpt-chat")],
        );
        assert_eq!(sel.counts(), (0, 2));
        sel.add("openai/*codex*").unwrap();
        assert!(state(&sel, "openai/gpt-codex").0);
        assert!(!state(&sel, "openai/gpt-chat").0);
        assert!(sel.add("openai/*codex*").is_err());
        sel.add("XAI/grok-9").unwrap();
        assert_eq!(state(&sel, "xai/grok-9"), (true, Status::Manual));
        sel.add("openai/gpt-chat").unwrap();
        assert!(state(&sel, "openai/gpt-chat").0);
        assert!(sel.add("not an id").is_err());
        assert_eq!(sel.remove_pattern(0), "openai/*codex*");
        assert!(!state(&sel, "openai/gpt-codex").0);
        let out = sel.to_scope();
        assert!(out.patterns.is_empty());
        assert_eq!(out.exceptions.len(), 2);
    }

    #[test]
    fn pattern_does_not_override_exclusion() {
        let s = scope(&[], &[("openai/gpt-codex", false)]);
        let mut sel = Selection::merge(Some(&s), &[], &[cand("openai/gpt-codex")]);
        sel.add("openai/*").unwrap();
        assert_eq!(state(&sel, "openai/gpt-codex"), (false, Status::Known));
    }

    #[test]
    fn remove_only_missing_or_manual() {
        let s = scope(&["a/*"], &[("a/gone", false)]);
        let mut sel = Selection::merge(Some(&s), &[known("a/old")], &[cand("a/here")]);
        let (g, i) = pos(&sel, "a/here");
        assert!(sel.toggle_remove(g, i).is_err());
        let (g, i) = pos(&sel, "a/gone");
        sel.toggle_remove(g, i).unwrap();
        let (g, i) = pos(&sel, "a/old");
        sel.toggle_remove(g, i).unwrap();
        assert!(sel.to_scope().exceptions.is_empty());
        assert_eq!(sel.removed_ids(), vec!["a/gone", "a/old"]);
        assert_eq!(sel.resolved().len(), 1);
    }

    #[test]
    fn changes_since_start() {
        let s = scope(&["a/*"], &[]);
        let before = Selection::merge(Some(&s), &[known("a/old")], &[cand("a/1"), cand("b/1")]);
        let mut sel = before.clone();
        assert!(sel.changes(&before).is_empty());

        let (g, i) = pos(&sel, "a/1");
        sel.toggle(g, i);
        let (g, i) = pos(&sel, "a/old");
        sel.toggle_remove(g, i).unwrap();
        sel.add("b/*").unwrap();
        sel.add("c/manual").unwrap();
        sel.remove_pattern(0);
        let c = sel.changes(&before);
        assert_eq!(c.patterns_added, vec!["b/*"]);
        assert_eq!(c.patterns_removed, vec!["a/*"]);
        assert_eq!(c.selected, vec!["b/1"]);
        assert_eq!(c.unselected, vec!["a/1"]);
        assert_eq!(c.added, vec!["c/manual"]);
        assert_eq!(c.removed, vec!["a/old"]);
        assert_eq!(c.exceptions, Some((0, 2)));
        assert_eq!(c.len(), 6);

        // Undoing every edit leaves no changes.
        let mut sel = before.clone();
        let (g, i) = pos(&sel, "a/1");
        sel.toggle(g, i);
        sel.toggle(g, i);
        assert!(sel.changes(&before).is_empty());
    }
}
