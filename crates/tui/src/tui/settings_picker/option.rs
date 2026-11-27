//! Declarative setting-option contract shared by settings pickers.
//!
//! Every picker row declares current/default/effective values, availability,
//! help/detail copy, and optional per-row actions. The option list is pure
//! data: hosts map [`super::PickerNavResult`] into their own preview/commit
//! actions, so a single option list can drive live preview without mutating
//! commit policy.

use std::borrow::Cow;

/// Triple of values a setting exposes for truthful chrome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingValues<T> {
    pub current: T,
    pub default: T,
    pub effective: T,
}

impl<T> SettingValues<T> {
    #[must_use]
    pub const fn new(current: T, default: T, effective: T) -> Self {
        Self {
            current,
            default,
            effective,
        }
    }
}

/// Whether a row can be selected / committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingAvailability {
    Available,
    #[cfg_attr(not(test), expect(dead_code))]
    // disabled rows with reasons for model/provider pickers (TUI-DOG-009)
    Disabled {
        reason: Cow<'static, str>,
    },
}

impl SettingAvailability {
    #[must_use]
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }

    #[must_use]
    #[cfg_attr(not(test), expect(dead_code))] // reason chrome for disabled rows (TUI-DOG-009)
    pub fn disabled_reason(&self) -> Option<&str> {
        match self {
            Self::Available => None,
            Self::Disabled { reason } => Some(reason.as_ref()),
        }
    }
}

/// Optional secondary action attached to a row (toggle, refresh, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingItemAction {
    pub id: Cow<'static, str>,
    pub label: Cow<'static, str>,
}

/// One selectable option in a settings picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingOption {
    pub id: Cow<'static, str>,
    pub label: Cow<'static, str>,
    /// Short secondary line shown in the list (tagline / summary).
    pub summary: Cow<'static, str>,
    /// Longer help shown in the detail pane.
    pub detail: Cow<'static, str>,
    pub help: Cow<'static, str>,
    pub values: SettingValues<Cow<'static, str>>,
    pub availability: SettingAvailability,
    pub tab: Cow<'static, str>,
    pub action: Option<SettingItemAction>,
    /// When true, narrow terminals drop the detail pane and keep the list.
    pub prefer_list_when_narrow: bool,
}

impl SettingOption {
    #[must_use]
    pub fn builder(
        id: impl Into<Cow<'static, str>>,
        label: impl Into<Cow<'static, str>>,
    ) -> SettingOptionBuilder {
        SettingOptionBuilder {
            id: id.into(),
            label: label.into(),
            summary: Cow::Borrowed(""),
            detail: Cow::Borrowed(""),
            help: Cow::Borrowed(""),
            values: SettingValues::new(Cow::Borrowed(""), Cow::Borrowed(""), Cow::Borrowed("")),
            availability: SettingAvailability::Available,
            tab: Cow::Borrowed("all"),
            action: None,
            prefer_list_when_narrow: true,
        }
    }
}

/// Fluent constructor for [`SettingOption`].
#[derive(Debug, Clone)]
pub struct SettingOptionBuilder {
    id: Cow<'static, str>,
    label: Cow<'static, str>,
    summary: Cow<'static, str>,
    detail: Cow<'static, str>,
    help: Cow<'static, str>,
    values: SettingValues<Cow<'static, str>>,
    availability: SettingAvailability,
    tab: Cow<'static, str>,
    action: Option<SettingItemAction>,
    prefer_list_when_narrow: bool,
}

impl SettingOptionBuilder {
    #[must_use]
    pub fn summary(mut self, summary: impl Into<Cow<'static, str>>) -> Self {
        self.summary = summary.into();
        self
    }

    #[must_use]
    pub fn detail(mut self, detail: impl Into<Cow<'static, str>>) -> Self {
        self.detail = detail.into();
        self
    }

    #[must_use]
    pub fn help(mut self, help: impl Into<Cow<'static, str>>) -> Self {
        self.help = help.into();
        self
    }

    #[must_use]
    pub fn values(mut self, values: SettingValues<Cow<'static, str>>) -> Self {
        self.values = values;
        self
    }

    #[must_use]
    pub fn availability(mut self, availability: SettingAvailability) -> Self {
        self.availability = availability;
        self
    }

    #[must_use]
    pub fn tab(mut self, tab: impl Into<Cow<'static, str>>) -> Self {
        self.tab = tab.into();
        self
    }

    #[must_use]
    #[cfg_attr(not(test), expect(dead_code))] // per-row secondary actions for model/provider hosts (TUI-DOG-009)
    pub fn action(mut self, action: SettingItemAction) -> Self {
        self.action = Some(action);
        self
    }

    #[must_use]
    pub fn prefer_list_when_narrow(mut self, prefer: bool) -> Self {
        self.prefer_list_when_narrow = prefer;
        self
    }

    #[must_use]
    pub fn build(self) -> SettingOption {
        SettingOption {
            id: self.id,
            label: self.label,
            summary: self.summary,
            detail: self.detail,
            help: self.help,
            values: self.values,
            availability: self.availability,
            tab: self.tab,
            action: self.action,
            prefer_list_when_narrow: self.prefer_list_when_narrow,
        }
    }
}
