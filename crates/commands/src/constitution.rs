use codewhale_command_contract::constitution::{
    CommandConstitutionContext, ConstitutionAction, ConstitutionLocale,
};
use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::model_setup::resolve_action;
use codewhale_command_contract::outcome::CommandResult;

const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "constitution",
    aliases: &["law"],
    usage: "/constitution [status|preview|base|suggestions|ratify <id>|migrate|rollback|bundled|edit|review|repair|repo|explain|posture|help]",
    description_key: "cmd_constitution_description",
};

pub struct ConstitutionCmd;

impl<A: TryFrom<ConstitutionAction>> RegisterCommand<CommandResult<A>> for ConstitutionCmd
where
    A::Error: std::fmt::Display,
{
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn handler() -> CommandHandler<CommandResult<A>> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::CONSTITUTION,
            handler: constitution,
        }
    }
}

fn constitution<A: TryFrom<ConstitutionAction>>(
    mut contexts: CommandContexts<'_>,
    arg: Option<&str>,
) -> CommandResult<A>
where
    A::Error: std::fmt::Display,
{
    let target = arg.map(str::trim).filter(|arg| !arg.is_empty());
    match target {
        None | Some("status" | "home" | "manager") => {
            return resolve_action(ConstitutionAction::OpenManager { review: false }, None);
        }
        Some("review" | "existing") => {
            return resolve_action(ConstitutionAction::OpenManager { review: true }, None);
        }
        Some("edit" | "guided" | "custom") => {
            return resolve_action(ConstitutionAction::OpenSetupConstitution, None);
        }
        Some("posture" | "runtime-posture") => {
            return resolve_action(ConstitutionAction::OpenSetupPosture, None);
        }
        Some("bundled" | "default" | "use-bundled" | "use-default") => {
            return resolve_action(ConstitutionAction::UseBundled, None);
        }
        Some("base" | "prompt" | "base-prompt" | "effective") => {
            return resolve_action(ConstitutionAction::PreviewEffectiveBasePrompt, None);
        }
        _ => {}
    }

    let Some(ctx) = contexts.take_constitution() else {
        return CommandResult::error("Command capability unavailable: constitution");
    };

    match target {
        Some("preview") => {
            let locale = ctx.locale();
            let text = format!("{}\n\n{}", preview_preamble(locale), ctx.preview_text());
            ctx.open_pager(rendered_title(locale), &text);
            CommandResult::ok()
        }
        Some("repo" | "repo-local" | "law") => {
            let locale = ctx.locale();
            let text = ctx
                .repo_constitution_block()
                .unwrap_or_else(|| no_repo_law_text(locale).to_string());
            ctx.open_pager(repo_title(locale), &text);
            CommandResult::ok()
        }
        Some("explain" | "agents") => {
            let locale = ctx.locale();
            ctx.open_pager(explanation_title(locale), agents_explanation(locale));
            CommandResult::ok()
        }
        Some("repair" | "fix") => {
            let message = ctx.repair_text();
            resolve_action(ConstitutionAction::OpenSetupConstitution, Some(message))
        }
        Some("suggestions" | "proposed") => {
            let locale = ctx.locale();
            ctx.show_pending_suggestions(suggestions_title(locale));
            CommandResult::ok()
        }
        Some("migrate") => CommandResult::message(ctx.migrate()),
        Some("rollback" | "undo-migrate") => CommandResult::message(ctx.rollback()),
        Some(rest) if rest.starts_with("ratify") => {
            let ids: Vec<&str> = rest
                .trim_start_matches("ratify")
                .split_whitespace()
                .collect();
            CommandResult::message(ctx.ratify_clauses(&ids))
        }
        Some("help") => CommandResult::message(help_text(ctx.locale())),
        Some(other) => CommandResult::error(format!(
            "Unknown /constitution target '{other}'. Try `/constitution` for the manager."
        )),
        None => unreachable!(),
    }
}

fn preview_preamble(locale: ConstitutionLocale) -> &'static str {
    match locale {
        ConstitutionLocale::ZhHans => {
            "这是本机宪章预览。已登录账户的宪章（包括默认值）优先；请在账户设置中预览。"
        }
        ConstitutionLocale::En => {
            "This previews the local constitution. The signed-in profile constitution, including its defaults, takes precedence; preview it in account settings."
        }
    }
}

fn rendered_title(locale: ConstitutionLocale) -> &'static str {
    match locale {
        ConstitutionLocale::ZhHans => "渲染后的用户宪章",
        ConstitutionLocale::En => "Rendered User Constitution",
    }
}

fn repo_title(locale: ConstitutionLocale) -> &'static str {
    match locale {
        ConstitutionLocale::ZhHans => "仓库本地宪章",
        ConstitutionLocale::En => "Repo-Local Constitution",
    }
}

fn explanation_title(locale: ConstitutionLocale) -> &'static str {
    match locale {
        ConstitutionLocale::ZhHans => "AGENTS.md 与宪章",
        ConstitutionLocale::En => "AGENTS.md vs Constitution",
    }
}

fn suggestions_title(locale: ConstitutionLocale) -> &'static str {
    match locale {
        ConstitutionLocale::ZhHans => "待批准的建议条款",
        ConstitutionLocale::En => "Suggested Clauses",
    }
}

fn no_repo_law_text(locale: ConstitutionLocale) -> &'static str {
    match locale {
        ConstitutionLocale::ZhHans => "此工作区未找到仓库本地宪章 .codewhale/constitution.json。",
        ConstitutionLocale::En => {
            "No repo-local constitution found at .codewhale/constitution.json for this workspace."
        }
    }
}

fn agents_explanation(locale: ConstitutionLocale) -> &'static str {
    match locale {
        ConstitutionLocale::ZhHans => {
            "\
AGENTS.md 与宪章

内置基础准则是精简的全局判断约定：身份、事实、验证、克制和优先级顺序。

用户全局宪章是个人长期偏好。它是结构化数据，确定性渲染，并低于当前用户请求和内置基础准则。

.codewhale/constitution.json 是仓库本地宪章。它属于某个工作区，并作为独立的仓库宪章块渲染。

AGENTS.md 和项目说明是项目规则/实现指导。它们可以描述构建命令、仓库规范和本地流程；按优先级顺序，它们低于当前用户请求和内置基础准则，高于用户全局长期偏好、记忆和交接。

WHALE.md 已忽略。将普通项目说明迁移到 AGENTS.md，将 Codewhale 专属权限策略迁移到 .codewhale/constitution.json。

运行时姿态是独立设置。宪章可以建议主动性，但不会改变批准策略、沙盒、Shell、网络、信任、MCP 权限或默认模式。使用 /constitution posture 查看这些控制。"
        }
        ConstitutionLocale::En => {
            "\
AGENTS.md vs constitution

The bundled Constitution is the compact global judgment contract: identity, ground truth, verification, restraint, and precedence.

The user-global constitution is personal standing preference law. It is structured, rendered deterministically, and subordinate to the current user request and the bundled Constitution.

.codewhale/constitution.json is repo-local law. It belongs to a workspace and is rendered as a separate repo constitution block.

AGENTS.md and project instructions are project law / implementation guidance. They can describe build commands, repository norms, and local workflows. Under “Whose word wins,” they sit below the current user request and bundled Constitution, and above user-global standing preferences, memory, and handoff.

WHALE.md is ignored. Move ordinary project instructions to AGENTS.md and Codewhale-specific authority policy to .codewhale/constitution.json.

Runtime posture is separate. A constitution can recommend autonomy, but it does not change approval policy, sandbox, shell, network, trust, MCP permissions, or default mode. Use /constitution posture to review those controls."
        }
    }
}

fn help_text(locale: ConstitutionLocale) -> String {
    match locale {
        ConstitutionLocale::ZhHans => "\
用法：/constitution [status|preview|base|suggestions|ratify <id>|migrate|rollback|bundled|edit|review|repair|repo|explain|posture|help]

常用命令：
- /constitution：打开宪章管理器和当前层级。
- /constitution preview：显示会注入模型的确切用户全局宪章块；缺失、空、无效或不可读时显示修复说明。
- /constitution edit：打开 /setup 的引导式宪章步骤；用 1-6 调整，按 G 预览，再按 G 保存。
- /constitution repair：说明当前文件状态，然后打开同一个引导式修复步骤。
- /constitution bundled：记录使用内置/默认准则，不创建自定义文件。
- /constitution repo：查看 .codewhale/constitution.json 仓库本地准则。
- /constitution explain：解释内置基础准则、用户全局宪章、仓库宪章、AGENTS.md、记忆和交接的区别。
- /constitution base：显示下一轮实际组装的确切基础提示词，附来源、摘要和字节/词元度量；只读，不发送请求，也不展开工具目录。
- /constitution suggestions：查看待批准的建议条款；它们不会注入模型，也不影响提示词缓存。
- /constitution ratify <id>：明确批准某条建议条款；若基线在审阅后发生变化则拒绝批准。
- /constitution migrate：把宪章文件迁移到当前架构版本并给出回执；被拒绝时文件保持原样。
- /constitution rollback：恢复迁移前的备份。
- /constitution posture：打开运行时姿态；宪章只提供模型指导，不会更改批准、沙盒、Shell、网络、信任或 MCP 权限。",
        ConstitutionLocale::En => "\
Usage: /constitution [status|preview|base|suggestions|ratify <id>|migrate|rollback|bundled|edit|review|repair|repo|explain|posture|help]

Common commands:
- /constitution: open the constitution manager and active stack.
- /constitution preview: show the exact user-global constitution block that would be injected; missing, empty, invalid, or unreadable files show repair guidance instead.
- /constitution edit: open the guided /setup Constitution step; tune 1-6, press G to preview, then G again to save.
- /constitution repair: explain the current file state, then open the same guided repair step.
- /constitution bundled: record bundled/default law without creating a custom file.
- /constitution repo: inspect repo-local .codewhale/constitution.json law.
- /constitution explain: compare Constitution, user-global law, repo law, AGENTS.md, memory, and handoff.
- /constitution base: show the exact effective base prompt assembled for the next turn, with per-block provenance, digests, and byte/token measures. Read-only: no provider request, no tool-catalog expansion.
- /constitution suggestions: review suggested clauses awaiting ratification; they are not injected and do not move the prompt-cache digest.
- /constitution ratify <id>: explicitly ratify a suggested clause. Refused if the base changed after you reviewed it.
- /constitution migrate: migrate the constitution file to the current schema with a receipt; a rejected file is left exactly as it was.
- /constitution rollback: restore the pre-migration backup.
- /constitution posture: open runtime posture; constitution text is model guidance only and does not change approvals, sandbox, shell, network, trust, or MCP permissions.",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockConstitutionContext {
        locale: ConstitutionLocale,
        pagers: Vec<(String, String)>,
        reviewed_digest: Option<String>,
        ratified_ids: Vec<String>,
    }

    impl Default for MockConstitutionContext {
        fn default() -> Self {
            Self {
                locale: ConstitutionLocale::En,
                pagers: Vec::new(),
                reviewed_digest: None,
                ratified_ids: Vec::new(),
            }
        }
    }

    impl CommandConstitutionContext for MockConstitutionContext {
        fn locale(&self) -> ConstitutionLocale {
            self.locale
        }
        fn open_pager(&mut self, title: &str, body: &str) {
            self.pagers.push((title.to_string(), body.to_string()));
        }
        fn preview_text(&self) -> String {
            "<codewhale_user_constitution/>".to_string()
        }
        fn repo_constitution_block(&self) -> Option<String> {
            None
        }
        fn show_pending_suggestions(&mut self, title: &str) {
            self.reviewed_digest = Some("digest-1".to_string());
            self.pagers.push((title.to_string(), "pending".to_string()));
        }
        fn ratify_clauses(&mut self, ids: &[&str]) -> String {
            self.ratified_ids = ids.iter().map(|s| (*s).to_string()).collect();
            format!("ratified:{}", ids.join(","))
        }
        fn migrate(&self) -> String {
            "migrated".to_string()
        }
        fn rollback(&self) -> String {
            "rolled back".to_string()
        }
        fn repair_text(&self) -> String {
            "repair guidance".to_string()
        }
    }

    fn run(
        ctx: &mut MockConstitutionContext,
        arg: Option<&str>,
    ) -> CommandResult<ConstitutionAction> {
        let CommandHandler::Contextual {
            capabilities,
            handler,
        } = ConstitutionCmd::handler()
        else {
            panic!("expected contextual handler");
        };
        assert_eq!(capabilities, CommandCapabilities::CONSTITUTION);
        let contexts = CommandContexts::empty().with_constitution(ctx);
        handler(contexts, arg)
    }

    #[test]
    fn action_subcommands_dispatch_without_host_context() {
        let CommandHandler::Contextual { handler, .. } = ConstitutionCmd::handler() else {
            panic!("expected contextual handler");
        };
        let res: CommandResult<ConstitutionAction> = handler(CommandContexts::empty(), None);
        assert_eq!(
            res.action,
            Some(ConstitutionAction::OpenManager { review: false })
        );
        let res: CommandResult<ConstitutionAction> =
            handler(CommandContexts::empty(), Some("bundled"));
        assert_eq!(res.action, Some(ConstitutionAction::UseBundled));
        let res: CommandResult<ConstitutionAction> =
            handler(CommandContexts::empty(), Some("base"));
        assert_eq!(
            res.action,
            Some(ConstitutionAction::PreviewEffectiveBasePrompt)
        );
    }

    #[test]
    fn contextual_subcommands_fail_without_capability() {
        let CommandHandler::Contextual { handler, .. } = ConstitutionCmd::handler() else {
            panic!("expected contextual handler");
        };
        let res: CommandResult<ConstitutionAction> =
            handler(CommandContexts::empty(), Some("preview"));
        assert!(res.is_error);
        assert_eq!(
            res.message.as_deref(),
            Some("Error: Command capability unavailable: constitution")
        );
    }

    #[test]
    fn suggestions_and_ratify_update_host_state() {
        let mut ctx = MockConstitutionContext::default();
        let res = run(&mut ctx, Some("suggestions"));
        assert!(!res.is_error);
        assert_eq!(ctx.reviewed_digest.as_deref(), Some("digest-1"));
        assert_eq!(ctx.pagers.len(), 1);

        let res = run(&mut ctx, Some("ratify c1 c2"));
        assert_eq!(res.message.as_deref(), Some("ratified:c1,c2"));
        assert_eq!(ctx.ratified_ids, vec!["c1", "c2"]);
    }
}
