# Sign in to a Codewhale account

You do not need an account to use Codewhale. Local use with your own provider
key or a local model works signed out; see [Providers](PROVIDERS.md). An
account adds things that live outside one machine: cloud computers, account
conversations, named agents, saved provider keys and machine tokens for CI.
Which of these your account has depends on the account and the deployed
service.

## Sign in

In the terminal UI:

```text
/login
```

From the shell:

```sh
codewhale login
```

Both open a browser page where you approve the sign-in. `codewhale login
--no-open` prints the address instead of opening a browser, for a remote
shell. The command waits up to 10 minutes; change that with
`--timeout-seconds`. In the terminal UI you can cancel the wait.

Signing in keeps your selected model and does not upload any key that is
already on this machine. Signing in alone does not give you model access:
connect a model with `/connect`, or keep using the one you have.

Check who you are signed in as:

```sh
codewhale account status
```

## Sign out

```text
/logout
```

```sh
codewhale logout
```

Sign-out removes the account session for the current `--profile` and revokes
it with the service when the service is reachable. Your local model
connections, settings and conversations stay.

`codewhale logout --all` is different: it removes every saved provider key,
provider sign-in and account session on this machine. It asks first; `--yes`
skips the question and is required when the command is not run from a
terminal.

## What stays on this machine

| Item | Where it is |
| --- | --- |
| Account session | The private Codewhale secrets file (mode `0600`), scoped to the profile and the account service address. It is not written to `config.toml` and does not use the operating-system keychain. |
| Provider keys you set locally | Your local configuration and secret store. Sign-in never uploads them. |
| Conversations, settings, memory | Local, unless you start a conversation on a cloud computer. |

Nothing is copied to the account automatically. `codewhale account push` is
never automatic and currently requires `--dry-run`, which shows what would be
pushed without writing it. `codewhale account pull --dry-run` shows the
account's copy without writing local files; importing it into local settings
is not available yet.

## Provider keys saved in the account

An account that supports saved provider keys can hold a key so that cloud
conversations can use it. These are keys Codewhale presents to a model
provider.

```sh
codewhale account keys list
codewhale account keys set deepseek --api-key-stdin
codewhale account keys set deepseek --from-local
codewhale account keys remove deepseek
```

- `list` shows which providers are configured. It never prints a key.
- `set` saves a key. `--api-key-stdin` reads it from standard input, so it
  does not appear in your shell history; `--from-local` uploads the key this
  machine already resolves for that provider. This is the only way a local key
  reaches the account.
- `remove` deletes the account's copy. It does not change the local copy or
  revoke the key at the provider.

## Machine tokens for CI

A machine token lets a script or CI job act as your account without a browser
sign-in. These are keys a caller presents to Codewhale.

```sh
codewhale account api-keys create --name "ci"
codewhale account api-keys list
codewhale account api-keys revoke TOKEN_ID
```

`create` prints the secret once; store it in your CI secret manager. Limit it
with `--expires-in-days` and one or more `--scope` values (`account:read`,
`agent:run`, `models:infer`; all three when omitted). `list` shows names and
ids, never the secret.

Set the token as `CODEWHALE_API_KEY` in the job. While it is set it is used
instead of the browser session, and `codewhale account whoami` reports the
account it authenticates as. Commands that manage computers, projects, GitHub
access and agents need an interactive sign-in and refuse a machine token.

The GitHub review workflow uses a machine token; see
[GitHub review bot](GITHUB_ACTION.md).

## More account commands

| Command | Use |
| --- | --- |
| `codewhale account computers` | List, create, pause and manage cloud computers. |
| `codewhale dispatch`, `codewhale account session` | Start or continue a conversation on a cloud computer. See [Cloud computer dispatch](CLOUD_DISPATCH.md). |
| `codewhale account agents` | Manage named agents and their saved model. |
| `codewhale account projects`, `codewhale account github` | List projects and the GitHub repositories the account may use. |

`codewhale cloud ...` is an older spelling of `codewhale account ...` and
still works. Run any command with `--help` for its options.
