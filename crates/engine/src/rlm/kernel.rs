use std::time::Duration;

use codewhale_dependencies::ExternalTool as _;
use codewhale_repl::PythonRuntime;

use crate::tools::spec::ToolContext;

const NO_INTERPRETER: &str = "no Python interpreter found on PATH (tried python3, python, py -3). \
                              Install Python 3 and restart codewhale.";

pub(crate) async fn spawn_kernel(
    tool_context: &ToolContext,
    context: Option<&str>,
    round_timeout: Option<Duration>,
) -> Result<PythonRuntime, String> {
    let interpreter = codewhale_dependencies::Python::resolve().ok_or(NO_INTERPRETER)?;
    let (program, fixed_args) = codewhale_dependencies::split_interpreter_spec(&interpreter);
    PythonRuntime::spawn(context, round_timeout, |kernel_args| {
        let mut args = fixed_args;
        args.extend(kernel_args);
        crate::tools::shell::sandboxed_runner_command(
            tool_context,
            &program,
            args,
            &tool_context.workspace,
            codewhale_repl::INLINE_ROUND_TIMEOUT,
        )
        .map_err(|error| error.to_string())
    })
    .await
}
