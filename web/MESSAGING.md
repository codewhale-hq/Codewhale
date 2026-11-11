# Codewhale website positioning

The words below are the site's positioning. Pages arrange them; they do not
rewrite them. Every locale carries the same words through the dictionary spine
(`lib/i18n/dictionaries/<locale>/home.ts`). `COPY_STYLE.md` owns the voice.

## The line

**The model can be anyone's. The agent should be yours.**

## The subhead

Codewhale is becoming the open environment where you build software with many
agents, on any model, on your own computer and in the cloud. It starts today in
your terminal and your browser, open source and yours to read.

The line under the install box reads "Version {version} is released, and it
needs no account." The version comes from the published release.

## The belief

The model you use is usually someone else's. You rent it, and it changes under
you. The agent is different. It reads your files, runs your commands, holds
what you are working on and acts in your name, so it should belong to you.
Codewhale is that agent. It is open source, so you can read every line of it,
or ask it to read itself to you. It works with any model you choose, you decide
what it may do without asking, and it leaves a record you can read. We are
building it into one place where you and many agents make software together,
from your terminal to a computer in the cloud, and it stays yours the whole way.

## The six sections

1. **Any model can work through your agent.** Use a built-in provider, any OpenAI-compatible endpoint or a model on your own machine, and change models between turns. Codewhale runs in your repository with no account, and `codewhale web` lets you work with it in your browser on your own machine.
2. **Your agent answers to you.** You decide what Codewhale may do without asking, and its commands can run in a sandbox on macOS and Linux. Every session leaves a receipt of the files it changed, the commands it ran and who approved each step.
3. **Several agents can share one task with you.** Codewhale splits a job across agents with roles such as explorer, implementer and reviewer, and you can watch, inspect and stop each one. A Fleet keeps a ledger, so a run can pick up again after a restart.
4. **Here is where your agent is going.** Codewhale is becoming one environment across your terminal, a desktop app, the browser and cloud computers, all run by one engine, so your work can move between them and stay yours. Each part below says where it stands today.
5. **Codewhale is built in the open by the people who use it.** It began as an open-source terminal tool for one model and became a project that hundreds of people in many languages build together. You can read its code, change it, or ask it to explain itself.
6. **Make your agent yours today.** Install Codewhale and run it in a repository with a model key you already have or a model on your own computer. You need no account, and `codewhale update` keeps it current.

## The whole product, labelled

| Part | State | Sentence |
| --- | --- | --- |
| Terminal app | Released | Install it on macOS, Linux or Windows and run `codewhale` in any repository. |
| Local browser client | Released | Run `codewhale web` to read the conversation, send a task and answer approvals in your browser; it comes with the terminal and needs no account. |
| Many agents on one task | Released | Sub-agents, Fleets and workflows let several agents share one task, each with its own role. |
| Desktop app | In development, no download | We are building a desktop app on the same engine, and it has no public download yet. |
| Cloud computers | Invite-only | Cloud computers are invite-only, and we are building them so a session can keep working on a machine in the cloud. |
| One session across them | Being built | We are building one session that goes with you from the terminal to the desktop, the browser and the cloud, and stays yours the whole way. |

The map is headed "One engine". The four states are Released, In development,
no download, Invite-only and Being built. A state is data, computed from the
typed surface states and the release facts in `lib/content/product-map.ts`,
never typed into a page. A part becomes Released on the day its tag and its
download exist. A sentence is written for one state; when a state changes, the
sentence changes with it. No dates anywhere.

## Churn rule

A change to the headline or subhead cites a new decision in CURRENT_DECISIONS
and names the proof it retires. No alternative headline is tried in git.

## Translator note

"Yours" means owned and controlled by the reader, not merely assigned to them.
Do not soften it to "for you". Commands, file names and product names stay in
Latin script and unchanged.
