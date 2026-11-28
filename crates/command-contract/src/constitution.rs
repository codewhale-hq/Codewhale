#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstitutionLocale {
    En,
    ZhHans,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstitutionAction {
    OpenManager { review: bool },
    OpenSetupConstitution,
    OpenSetupPosture,
    UseBundled,
    PreviewEffectiveBasePrompt,
}

pub trait CommandConstitutionContext {
    fn locale(&self) -> ConstitutionLocale;
    fn open_pager(&mut self, title: &str, body: &str);
    fn preview_text(&self) -> String;
    fn repo_constitution_block(&self) -> Option<String>;
    fn show_pending_suggestions(&mut self, title: &str);
    fn ratify_clauses(&mut self, ids: &[&str]) -> String;
    fn migrate(&self) -> String;
    fn rollback(&self) -> String;
    fn repair_text(&self) -> String;
}
