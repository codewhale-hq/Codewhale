# Engine run completion

`EngineHandle::wait_run_completion` observes the particular background run
created by either engine spawn factory. `NormalReturn` means that run returned
and its owned future was dropped. `AbnormalTermination` covers panic,
run cancellation, or loss of the observation sender without a terminal value.
Handles constructed directly with `Engine::new` return `NotObserved`.

Each waiter receives the same terminal value. Cancelling one wait neither stops
the engine nor consumes another waiter's result. Waiting does not initiate
shutdown; callers must use the existing shutdown protocol. The observation
does not keep another engine or operation-mailbox sender alive.

The one-shot exec consumer shares its existing two-second absolute deadline
between sending `Shutdown` and waiting for completion after the event loop.
An unconfirmed normal return produces a warning without changing the task's
result. Existing early error returns and broken-pipe process exits do not pass
through this cleanup. The deadline bounds cooperative asynchronous waits; it
cannot preempt a blocking poll or destructor.

Completion does not certify durable persistence, successful work, or termination
of detached children, blocking writers, event forwarders, or the supervisor's
later diagnostics.
