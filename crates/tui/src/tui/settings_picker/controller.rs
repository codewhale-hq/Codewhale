//! Stable filter/index ownership for transactional settings pickers.
//!
//! The controller owns the option catalog, search query, tab filter, and the
//! mapping from visible row → source index. Callers never re-filter ad hoc
//! during render; they read [`SettingsPickerController::visible`] instead.

use super::option::{SettingAvailability, SettingOption};

/// Outcome of a navigation or commit attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerNavResult {
    /// Selection changed; host should run preview.
    Preview,
    /// Enter on an available option; host should commit.
    Commit,
    /// Esc / explicit cancel; host should rollback then close.
    Cancel,
    /// Secondary action on the focused row.
    ItemAction,
    /// No-op (disabled row, empty list, unrecognized key).
    None,
}

/// Tab + search + selection state with stable filtered indices.
#[derive(Debug, Clone)]
pub struct SettingsPickerController {
    options: Vec<SettingOption>,
    tabs: Vec<String>,
    active_tab: usize,
    query: String,
    /// Indices into `options` that pass the current tab + search filter.
    filtered: Vec<usize>,
    /// Index into `filtered` (not into `options`).
    selected_visible: usize,
    /// Snapshot of the selection id when the picker opened (for rollback).
    original_id: String,
}

impl SettingsPickerController {
    #[must_use]
    pub fn new(options: Vec<SettingOption>, original_id: impl Into<String>) -> Self {
        let mut tabs = vec!["all".to_string()];
        for option in &options {
            let tab = option.tab.as_ref();
            if tab != "all" && !tabs.iter().any(|existing| existing == tab) {
                tabs.push(tab.to_string());
            }
        }
        let mut controller = Self {
            options,
            tabs,
            active_tab: 0,
            query: String::new(),
            filtered: Vec::new(),
            selected_visible: 0,
            original_id: original_id.into(),
        };
        controller.recompute_filter(None);
        // Prefer landing on the original id when it exists.
        if !controller.original_id.is_empty() {
            let original = controller.original_id.clone();
            controller.recompute_filter(Some(&original));
        }
        controller
    }

    #[must_use]
    pub fn options(&self) -> &[SettingOption] {
        &self.options
    }

    #[must_use]
    #[allow(dead_code)] // tab strip for multi-tab pickers (TUI-DOG-009)
    pub fn tabs(&self) -> &[String] {
        &self.tabs
    }

    #[must_use]
    #[allow(dead_code)] // active tab index for hosts that paint the strip (TUI-DOG-009)
    pub fn active_tab(&self) -> usize {
        self.active_tab
    }

    #[must_use]
    pub fn active_tab_name(&self) -> &str {
        self.tabs
            .get(self.active_tab)
            .map(String::as_str)
            .unwrap_or("all")
    }

    #[must_use]
    #[allow(dead_code)] // search query accessor for host chrome (TUI-DOG-009)
    pub fn query(&self) -> &str {
        &self.query
    }

    #[must_use]
    #[allow(dead_code)] // identity accessor for the theme-picker controller test
    pub fn original_id(&self) -> &str {
        &self.original_id
    }

    /// Visible source indices after tab + search filtering.
    #[must_use]
    pub fn visible(&self) -> &[usize] {
        &self.filtered
    }

    #[must_use]
    pub fn tab_match_counts(&self) -> Vec<(String, usize)> {
        let query = self.query.to_ascii_lowercase();
        let matches = |option: &SettingOption| {
            query.is_empty()
                || option.id.to_ascii_lowercase().contains(&query)
                || option.label.to_ascii_lowercase().contains(&query)
                || option.summary.to_ascii_lowercase().contains(&query)
                || option.detail.to_ascii_lowercase().contains(&query)
        };
        self.tabs
            .iter()
            .map(|tab| {
                let count = self
                    .options
                    .iter()
                    .filter(|option| {
                        (tab == "all" || option.tab.as_ref() == tab) && matches(option)
                    })
                    .count();
                (tab.clone(), count)
            })
            .collect()
    }

    #[must_use]
    pub fn selected_visible(&self) -> usize {
        self.selected_visible
    }

    #[must_use]
    pub fn selected_source_index(&self) -> Option<usize> {
        self.filtered.get(self.selected_visible).copied()
    }

    #[must_use]
    pub fn selected_option(&self) -> Option<&SettingOption> {
        self.selected_source_index()
            .and_then(|idx| self.options.get(idx))
    }

    #[must_use]
    pub fn selected_id(&self) -> Option<&str> {
        self.selected_option().map(|option| option.id.as_ref())
    }

    /// Recompute the filtered index list, optionally preserving a source id.
    ///
    /// A non-empty search query searches across every tab so operators are not
    /// trapped inside the active tab while typing.
    pub fn recompute_filter(&mut self, prefer_source_id: Option<&str>) {
        let tab = self.active_tab_name().to_string();
        let query = self.query.to_ascii_lowercase();
        let search_all_tabs = !query.is_empty();
        self.filtered = self
            .options
            .iter()
            .enumerate()
            .filter(|(_, option)| {
                (search_all_tabs || tab == "all" || option.tab.as_ref() == tab)
                    && (query.is_empty()
                        || option.id.to_ascii_lowercase().contains(&query)
                        || option.label.to_ascii_lowercase().contains(&query)
                        || option.summary.to_ascii_lowercase().contains(&query)
                        || option.detail.to_ascii_lowercase().contains(&query))
            })
            .map(|(idx, _)| idx)
            .collect();

        if let Some(id) = prefer_source_id
            && let Some(visible) = self.filtered.iter().position(|&source| {
                self.options
                    .get(source)
                    .is_some_and(|o| o.id.as_ref() == id)
            })
        {
            self.selected_visible = visible;
            return;
        }

        if self.filtered.is_empty() {
            self.selected_visible = 0;
        } else {
            self.selected_visible = self
                .selected_visible
                .min(self.filtered.len().saturating_sub(1));
        }
    }

    #[allow(dead_code)] // bulk query replace for search hosts (TUI-DOG-009)
    pub fn set_query(&mut self, query: impl Into<String>) {
        let keep = self.selected_id().map(str::to_string);
        self.query = query.into();
        self.recompute_filter(keep.as_deref());
    }

    pub fn push_query_char(&mut self, ch: char) {
        let keep = self.selected_id().map(str::to_string);
        self.query.push(ch);
        self.recompute_filter(keep.as_deref());
    }

    pub fn pop_query_char(&mut self) {
        let keep = self.selected_id().map(str::to_string);
        self.query.pop();
        self.recompute_filter(keep.as_deref());
    }

    pub fn clear_query(&mut self) {
        let keep = self.selected_id().map(str::to_string);
        self.query.clear();
        self.recompute_filter(keep.as_deref());
    }

    pub fn set_active_tab(&mut self, tab_idx: usize) {
        if tab_idx >= self.tabs.len() {
            return;
        }
        let keep = self.selected_id().map(str::to_string);
        self.active_tab = tab_idx;
        self.recompute_filter(keep.as_deref());
    }

    /// Switch to the next tab. Previews the row it lands on only when that
    /// row is available — a disabled row is shown, never previewed.
    pub fn next_tab(&mut self) -> PickerNavResult {
        if self.tabs.is_empty() {
            return PickerNavResult::None;
        }
        let next = (self.active_tab + 1) % self.tabs.len();
        self.set_active_tab(next);
        self.preview_if_available()
    }

    /// Switch to the previous tab; same preview rule as [`Self::next_tab`].
    pub fn prev_tab(&mut self) -> PickerNavResult {
        if self.tabs.is_empty() {
            return PickerNavResult::None;
        }
        let prev = (self.active_tab + self.tabs.len() - 1) % self.tabs.len();
        self.set_active_tab(prev);
        self.preview_if_available()
    }

    /// Select by source index when that option is currently visible.
    pub fn select_source_index(&mut self, source: usize) -> PickerNavResult {
        let Some(visible) = self.filtered.iter().position(|&idx| idx == source) else {
            return PickerNavResult::None;
        };
        self.selected_visible = visible;
        if self
            .selected_option()
            .is_some_and(|o| o.availability.is_available())
        {
            PickerNavResult::Preview
        } else {
            PickerNavResult::None
        }
    }

    pub fn move_up(&mut self) -> PickerNavResult {
        if self.filtered.is_empty() {
            return PickerNavResult::None;
        }
        self.selected_visible =
            (self.selected_visible + self.filtered.len() - 1) % self.filtered.len();
        self.preview_if_available()
    }

    pub fn move_down(&mut self) -> PickerNavResult {
        if self.filtered.is_empty() {
            return PickerNavResult::None;
        }
        self.selected_visible = (self.selected_visible + 1) % self.filtered.len();
        self.preview_if_available()
    }

    pub fn jump_home(&mut self) -> PickerNavResult {
        if self.filtered.is_empty() {
            return PickerNavResult::None;
        }
        self.selected_visible = 0;
        self.preview_if_available()
    }

    pub fn jump_end(&mut self) -> PickerNavResult {
        if self.filtered.is_empty() {
            return PickerNavResult::None;
        }
        self.selected_visible = self.filtered.len().saturating_sub(1);
        self.preview_if_available()
    }

    /// 1-indexed digit jump into the *visible* list.
    pub fn jump_digit(&mut self, digit: u8) -> PickerNavResult {
        if !(1..=9).contains(&digit) || self.filtered.is_empty() {
            return PickerNavResult::None;
        }
        let idx = usize::from(digit - 1);
        if idx >= self.filtered.len() {
            return PickerNavResult::None;
        }
        self.selected_visible = idx;
        self.preview_if_available()
    }

    pub fn request_commit(&self) -> PickerNavResult {
        match self.selected_option() {
            Some(option) if option.availability.is_available() => PickerNavResult::Commit,
            _ => PickerNavResult::None,
        }
    }

    pub fn request_cancel(&self) -> PickerNavResult {
        PickerNavResult::Cancel
    }

    pub fn request_item_action(&self) -> PickerNavResult {
        match self.selected_option() {
            Some(option) if option.action.is_some() && option.availability.is_available() => {
                PickerNavResult::ItemAction
            }
            _ => PickerNavResult::None,
        }
    }

    fn preview_if_available(&self) -> PickerNavResult {
        match self.selected_option() {
            Some(option) if option.availability.is_available() => PickerNavResult::Preview,
            Some(SettingOption {
                availability: SettingAvailability::Disabled { .. },
                ..
            }) => PickerNavResult::None,
            _ => PickerNavResult::None,
        }
    }
}
