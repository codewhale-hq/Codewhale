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
base. The greater of the local delay and the server minimum is used, with
additional jitter of half the local delay when a nonzero minimum is present.

The legacy `[retry]` backoff fields (`initial_delay`, `max_delay`,
`exponential_base`, `jitter`, `jitter_factor` and `respect_retry_after`) do not
override this 429 policy. In particular, `respect_retry_after = false` does not
suppress server minima and `jitter = false` does not disable rate-limit jitter.
`max_retries` counts ordinary transport retries; `max_retries = 0` alone does
not disable 429 recovery. Use `retry.rate_limit.max_attempts = 1` to disable only
automatic 429 retries, or `retry.enabled = false` to disable this HTTP retry loop.
The existing safe pre-dispatch HTTP/1.1 fallback inside a stream-opening operation
remains possible.

All four rate-limit fields are validated when configuration loads:
`max_attempts` must be `1..=1000`, `recovery_budget_secs` must be `1..=86400`,
and `1 <= initial_delay_secs <= max_delay_secs <= 86400`. Invalid values fail
loading rather than silently changing these bounds. The actual pause/sleep
limit below still applies even when a longer recovery budget is configured.

Numeric and HTTP-date Retry-After values and recognized JSON reset fields are
supported. A minimum beyond the remaining recovery budget is returned to the
caller immediately, without shortening it to the local backoff ceiling. Native
MiniMax/Zhipu JSON success envelopes carrying a failure use the same policy and
the caller's existing error-disclosure boundary. Known quota windows, billing
and invalid-input failures are not short-retried. Server-directed provider pauses and
automatic retry sleeps are limited to 3600 seconds, including sub-agents. Longer
minima remain attached to the returned error as advice; the current operation
stops rather than retrying before that minimum. Quota-window errors distinguish
a server rejection from admission that did not dispatch a request and do not
claim that this transport slice saved a checkpoint.

Backoff does not consume the active HTTP envelope. The request's existing body
timeout still bounds a stalled non-streaming response. Native JSON envelope
inspection has a 32 MiB size limit and applies the caller's first-byte and inter-chunk idle deadlines. Ordinary SSE body and idle recovery
remain owned by the existing stream adapter. Dropping the operation
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
