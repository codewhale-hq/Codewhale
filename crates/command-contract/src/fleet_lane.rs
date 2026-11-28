#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlOperationHelp {
    pub slash_invocation: String,
    pub authority: String,
    pub summary: String,
    pub cli_invocation: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlReceiptOutcome {
    pub rendered: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneControlVerb {
    List,
    Status,
    Interrupt,
    Restart,
    Resume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FleetControlVerb {
    List,
    Status,
    Interrupt,
    Restart,
    Resume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FleetRouteSaveKind {
    UpdateFleet,
    SaveAsNewFleet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FleetUsageKind {
    Add,
    Remove,
}

pub trait CommandLaneContext {
    fn control_operations(&self) -> Vec<ControlOperationHelp>;
    fn resolve_control_verb(&self, verb: &str) -> Option<LaneControlVerb>;
    fn execute_control(
        &mut self,
        verb: LaneControlVerb,
        target: Option<&str>,
    ) -> ControlReceiptOutcome;
}

pub trait CommandFleetContext {
    fn control_operations(&self) -> Vec<ControlOperationHelp>;
    fn resolve_control_verb(&self, verb: &str) -> Option<FleetControlVerb>;
    fn run_control(
        &mut self,
        verb: FleetControlVerb,
        target: Option<&str>,
    ) -> ControlReceiptOutcome;
    fn fleet_models_text(&self) -> String;
    fn save_route(&mut self, kind: FleetRouteSaveKind) -> String;
    fn usage_text(&self, kind: FleetUsageKind) -> String;
    fn prepare_workers_view(&mut self);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetAction {
    OpenRoster,
    OpenSetup,
    OpenList,
    OpenWorkers,
    AddModel {
        provider: String,
        model: String,
        roles: Vec<String>,
    },
    RemoveModel {
        provider: String,
        model: String,
    },
}
