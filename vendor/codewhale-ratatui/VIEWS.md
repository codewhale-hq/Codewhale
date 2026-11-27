# Codewhale terminal views as reusable Ratatui parts

Use the parts of Codewhale's terminal interface in your own app. The library
accepts your display data, draws into a Ratatui buffer, and leaves application
actions with you. You can use the native composer by itself, put the workbar
beside your conversation, or build a complete picker with the instrument shell.

The source reference for this guide is
[`Hmbown/CodeWhale` at `a79ce5c4d5ed1a5f7032185710c27343a900351c`](https://github.com/Hmbown/CodeWhale/tree/a79ce5c4d5ed1a5f7032185710c27343a900351c/crates/tui/src/tui).
Paths in the catalog below are relative to `crates/tui/src/tui/` in that tree.

## Start with an extracted renderer

| Part | Current native source | What you supply |
| --- | --- | --- |
| `InstrumentSurface` | `views/mod.rs::render_underwater_surface`, action-footer helpers | Title and key/action labels |
| `SessionList`, `SessionRow` | `session_picker.rs::build_list_lines`, `format_session_line` | Filtered and sorted sessions, selected row, timestamp text |
| `NativeComposer` | `widgets/mod.rs`, `composer_chrome.rs` | Draft, focus, submission state, hint and target |
| `PostureBar` | `phase_strip.rs::TidelineFooter` | Permission, mode, clocks, counts and interrupt hint |
| `MetricsLine` | `infoline.rs::InfoLine` | Model, context, costs, timing and optional metrics |
| `Workbar` | `work_surface/{model,input,render}` | Tasks, agents, jobs, files, notes and workspace facts |
| `WorkflowProgress` | `widgets/workbar.rs` | Workflow runs, outcomes, elapsed time and tokens |
| `TerminalShell` | `ui/frame.rs` | Sizes for transcript, pending input, composer and footer slots |
| `OceanColumn`, `Habitat` | `ocean.rs`, `ambient_life.rs` | Time, motion policy, phase and context percentage |
| `Theme::tui`, `TuiPalette` | `crates/palette/src/` | Terminal capabilities and the selected native theme |

These retain the native layout or authored visual behavior while adapting
application types into portable display facts. `InstrumentSurface` uses a
one-cell horizontal margin at 44 columns and a one-cell vertical margin at 24
rows. Its title sits on the top rule; whole action hints wrap along the bottom.
`SessionList` keeps the numbered shortcuts, current/fork/archived labels,
32-cell title cap and native scroll rail. Your app loads sessions and handles
resume, search, rename and deletion.

The current main terminal has a conversation followed by pending input,
composer, posture, workflow progress, metrics and the workbar. Its default
layout has no top header or permanent Files sidebar. `Workbar` is the tabbed
Tasks/Fleet/Jobs/Files/Notes/Context/Git/Cost dock; `WorkflowProgress` is the
separate, borderless workflow strip.

```rust
use codewhale_ratatui::{
    InstrumentSurface, KeyHint, Paint, PickerState, SessionList, SessionRow,
    Theme,
};

let theme = Theme::detect().tui();
let sessions = SessionList::new(vec![
    SessionRow::new("4a80c752-28b1", "Review authentication")
        .messages(18)
        .mode("Work")
        .updated("2026-10-01 14:12 (2m ago)")
        .current(true),
]).state(PickerState::new(0));

let surface = InstrumentSurface::new("sessions").actions(vec![
    KeyHint::new("Enter", "resume"),
    KeyHint::new("/", "search"),
    KeyHint::new("Esc", "close"),
]);

// In your rendering callback:
// let body = surface.draw(area, buffer, &theme);
// sessions.paint(body, buffer, &theme);
// Use sessions.hitboxes(body, &theme) to connect mouse selection to your state.
```

## Native view recipes

Run `cargo run --example gallery` and choose a recipe. The recipe functions in
[`src/gallery/native_views.rs`](src/gallery/native_views.rs) are also small
integration examples you can adapt. These are **example compositions** of
native templates and caller data, rather than exported application controllers.

| Recipe | Native shape and reusable parts |
| --- | --- |
| `view-sessions`, `view-sessions-narrow`, `view-sessions-compact`, `view-sessions-empty` | `InstrumentSurface` + `SessionList`; history left 56%, list right 44%; narrow rooms stack, short rooms retain the list |
| `view-settings`, `view-settings-narrow` | Native Settings category/search header, Display rows, optional groups and fact inspector; footer preview uses `PostureBar` |
| `view-commands` | Native centered palette; filter/count/scope header, section heading and aligned command descriptions |
| `view-models`, `view-models-narrow` | Instrument title/catalog action, provider line, model table and reasoning pane; native list/detail split and 30-cell effort pane |
| `view-providers` | Instrument title/catalog action, active provider marker and list/detail layout |
| `view-theme` | Native selectable theme order, numbered choices and five swatches, including the Underwater water column |
| `view-mode` | Native centered choice: Work, Plan, Operate, each on one row with its original hint |
| `view-status` | Native centered footer configuration with checkboxes and the current status-item labels |
| `view-file-picker` | Instrument with the `@ attach` match title, query, relevance field and paths |
| `view-fleet-dock`, `view-jobs-dock`, `view-files-dock`, `view-context-dock`, `view-git-dock`, `view-cost-dock` | Native `Workbar` panels in a side placement beside conversation and composer |

The Settings recipe supplies five Appearance rows. Model/provider recipes
supply a small configured list. Catalog discovery, provider sign-in, connection
tests, routing, settings persistence, session preview parsing and filesystem
scanning belong to the host. The fleet **dock** recipe demonstrates the workbar
Fleet panel; the separate `/fleet` roster remains a host-owned room.

## Every current modal

The current `ModalKind` enum has **33** variants. “Recipe reuse” means that the
named preview shows reusable visual parts for that room, not a copy of its
runtime or every state. Application-specific rooms remain in Codewhale; this
catalog connects them to the kit without introducing another controller.

<!-- modal-inventory:start -->
| Modal kind | Native source | Reusable parts | Gallery recipe / coverage |
| --- | --- | --- | --- |
| `PetHabitat` | `pet_watch/habitat.rs`, `ambient_life.rs` | `Habitat`, `FishSchool`, `Shoal`, `Jellyfish`, `BubbleField`, `MarineSnow`, `DistantWhale`, `Whale` | `habitat-scene`: extracted marine visuals; pet interaction belongs to the host |
| `Approval` | `approval/view.rs` | `ApprovalCard`, `ApprovalState`, `KeyHint` | `approval-command`, `approval-patch`: approval composition; host owns policy |
| `Elevation` | `approval/elevation.rs` | `ApprovalCard`, `KeyHint` | `approval-elevation`: recipe reuse for the elevation subject and choices |
| `UserInput` | `user_input.rs` | `Form`, `TextInput`, `Picker`, `KeyHint` | `form`: recipe reuse; host supplies the question schema and answers |
| `CommandPalette` | `command_palette.rs` | `InstrumentSurface::draw_footer`, `List`, `Theme` | `view-commands`: native palette composition |
| `Help` | `views/help.rs` | `KeyHints`, `Keymap`, `List`, `Transcript` | `view-commands`, `transcript-prose`: recipe reuse for discovery and grouped help; host supplies commands |
| `SubAgents` | `views/mod.rs::SubAgentsView`, `agent_details.rs` | `Workbar`, `AgentCard`, `Transcript` | `workbar-fleet`, `view-fleet-dock`: extracted agent rows; full manager and details are host-owned |
| `Pager` | `pager.rs`, `agent_details.rs` | `Transcript`, `CodeBlock`, `InstrumentSurface`, `Diff` | `transcript-prose`, `transcript-code`, `diff`: content recipe reuse; host owns paging and copy |
| `LiveTranscript` | `live_transcript.rs` | `Transcript`, `Message`, `ToolCard`, `TerminalShell` | `transcript-prose`, `showcase-work`: content recipe reuse; host streams and scrolls |
| `SessionPicker` | `session_picker.rs` | `InstrumentSurface`, `SessionList`, `SessionRow`, `PickerState` | `view-sessions` and its three responsive variants: native composition and extracted rows |
| `Config` | `views/mod.rs::ConfigView` | `InstrumentSurface`, `Theme`, `PostureBar`, setting display facts | `view-settings`, `view-settings-narrow`: native Settings shell composition |
| `ModelPicker` | `model_picker.rs` | `InstrumentSurface`, `List`, `Theme` | `view-models`, `view-models-narrow`: configured-model composition; host owns catalog, effort choices and apply |
| `ProviderPicker` | `provider_picker.rs` | `InstrumentSurface`, `List`, `Theme` | `view-providers`: native provider-list composition; host owns credentials and discovery |
| `ModePicker` | `views/mode_picker.rs` | `List`, `ListRow`, `InstrumentSurface::draw_footer`, `centered` | `view-mode`: native three-mode composition |
| `FleetRoster` | `views/fleet_roster.rs` | `List`, `AgentCard`, `InstrumentSurface::draw_footer` | `view-fleet-dock`: agent-fact recipe reuse, not the separate operator/member roster |
| `FleetSetup` | `views/fleet_setup.rs` | `Form`, `TextInput`, `Picker`, `KeyHint` | `form`, `view-providers`: editing and route-choice recipe reuse; host saves team definitions |
| `FleetList` | `views/fleet_list.rs` | `InstrumentSurface`, `List`, `EmptyState` | `view-sessions`, `view-fleet-dock`: list/agent recipe reuse; host owns team selection |
| `FleetDetail` | `views/fleet_detail.rs` | `InstrumentSurface`, `List`, `AgentCard`, `Transcript` | `view-fleet-dock`, `transcript-prose`: member/detail recipe reuse |
| `HotbarSetup` | `hotbar/setup.rs` | `Form`, `Picker`, `KeyHint` | `form`, `view-mode`: recipe reuse for bound choices; host owns hotbar commands |
| `SetupWizard` | `setup/mod.rs` | `Form`, `TextInput`, `Picker`, `NativeComposer` | `form`, `view-providers`, `native-composer`: setup building blocks; host controls steps |
| `FilePicker` | `file_picker.rs` | `InstrumentSurface`, `Theme`, text fitting | `view-file-picker`: native path-picker composition; host supplies scan results and relevance |
| `StatusPicker` | `views/status_picker.rs`, `../config.rs::StatusItem` | `InstrumentSurface::draw_footer`, `Theme`, `centered`, `MetricsLine` | `view-status`: native checkbox composition; host stores the chosen fields |
| `FeedbackPicker` | `feedback_picker.rs` | `Form`, `TextInput`, `Picker`, `KeyHint` | `form`, `text-input-typed`: feedback-input recipe reuse; submission belongs to the host |
| `ThemePicker` | `theme_picker.rs`, `crates/palette/src/ids.rs` | `InstrumentSurface`, `TuiPalette`, `Theme`, `OceanRamp` | `view-theme`: native theme-list composition; host previews, saves and reverts |
| `ContextMenu` | `context_menu.rs` | `List`, `Picker`, `KeyHint`, `Theme` | `view-commands`: action-choice recipe reuse; host supplies the target's actions |
| `ContextInspector` | `context_inspector.rs` | `InstrumentSurface`, `List`, `Transcript`, `Workbar` | `view-context-dock`, `instrument-surface`: context-fact recipe reuse; host provides the source map |
| `SkillsManager` | `views/skills_manager.rs` | `InstrumentSurface`, `List`, `SettingDetail`, `EmptyState` | `view-providers`: list/inspector recipe reuse; host discovers and enables skills |
| `Extensions` | `views/extensions.rs` | `InstrumentSurface`, `List`, `Transcript`, `EmptyState` | `view-providers`, `view-commands`: inventory/discovery recipe reuse; mutations remain in host controllers |
| `WorktreeManager` | `worktree_manager.rs` | `InstrumentSurface`, `List`, `Form`, `Diff` | `view-sessions`, `diff`: list/comparison recipe reuse; host owns Git operations |
| `WorkflowsManager` | `views/workflows_manager.rs` | `InstrumentSurface`, `List`, `WorkflowProgress`, `WorkflowTree` | `workflow-progress-live`, `workflow-tree`: run-content recipe reuse; host reads the journal and cancels runs |
| `Automations` | `views/automations.rs` | `InstrumentSurface`, `List`, `Form`, `KeyHint` | `view-sessions`, `form`: schedule-list/editor recipe reuse; host owns scheduling and mutations |
| `LaunchResumeConfirm` | `launch_resume_confirm.rs` | `Dialog`, `KeyHint`, `SessionRow` | `dialog`, `session-list`: confirmation recipe reuse; host performs the resume |
| `RouterSetup` | `views/router_setup.rs` | `InstrumentSurface`, `Form`, `Picker`, `KeyHint` | `view-models`, `form`: route/choice recipe reuse; host owns router tests and configuration |
<!-- modal-inventory:end -->

## The work dock is a view family too

`work_surface/model.rs` projects all eight tabs into row facts;
`work_surface/render/mod.rs` paints them.
`work_surface/render/rows.rs` and `layout.rs` own their columns and placement.
The kit preserves those renderers instead of giving each tab a new card design.

| Native work panel | Kit part | Gallery |
| --- | --- | --- |
| Tasks | `WorkbarPanel::Tasks`, task marks, goal and progress | `workbar-tasks` |
| Fleet | `WorkbarPanel::Fleet`, `WorkbarAgent` with responsive columns | `workbar-fleet`, `view-fleet-dock` |
| Jobs | `WorkbarPanel::Jobs`, status and job details | `workbar-jobs`, `view-jobs-dock` |
| Files | `WorkbarPanel::Files`, grouped edited/read rows | `workbar-files`, `view-files-dock` |
| Notes | `WorkbarPanel::Notes`, note text and actions | `workbar-notes` |
| Context | `WorkbarPanel::Context`, budget and source facts | `workbar-context`, `view-context-dock` |
| Git | `WorkbarPanel::Git`, branch, changes and commit facts | `workbar-git`, `view-git-dock` |
| Cost | `WorkbarPanel::Cost`, session/agent/cache facts | `workbar-cost`, `view-cost-dock` |

Choose bottom, top, left or right placement. The same public row types work in
each placement. Host input selects the row and dispatches its action; the
library does not run tools, inspect Git, read files, or infer prices.
