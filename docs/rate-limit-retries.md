# HTTP rate-limit retries

`[retry.rate_limit]` configures the native HTTP transport's recovery loop:

```toml
[retry.rate_limit]
max_attempts = 20
recovery_budget_secs = 600
initial_delay_secs = 5
max_delay_secs = 60
```

These are engineering defaults. They do not assert a provider-wide recovery
percentile. The budget starts at the first rate refusal, counts the initial HTTP
attempt, and is independent of the existing network/5xx retry count. Backoff
doubles to a local ceiling of 60 seconds, with equal jitter over 50–100% of the
base. The greater of the local delay and the server minimum is used.

Numeric and HTTP-date Retry-After values and recognized JSON reset fields are
supported. A minimum beyond the remaining recovery budget is returned to the
caller immediately, without shortening it to the local backoff ceiling. Native
MiniMax/Zhipu JSON success envelopes carrying a failure use the same policy and
the caller's existing error-disclosure boundary. Known quota windows, billing
and invalid-input failures are not short-retried.

Backoff does not consume the active HTTP envelope. The request's existing body
timeout still bounds a stalled non-streaming response. Streaming body and idle
recovery remain owned by the existing stream adapter. Dropping the operation
cancels its wait. `retry.enabled = false` still runs a transport operation once.

This contribution changes the common native HTTP transport loop, its isolated
preview classifier, retry clock and typed errors. The separately implemented
legacy vision adapter is outside this transport slice; no shared task/admission
workflow is claimed for that adapter. It does
not introduce a task scheduler, persisted checkpoints, host UI events, shared
task-operation budgets across outer stream retries, or a new task outcome
policy. Those remain responsibilities of the calling engine/host. A successful
transport retry is not evidence that a host's complete task recovery workflow
has been verified.
