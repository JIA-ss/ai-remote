# Operating ai-remote

[中文](operations.zh-CN.md) | English

This guide is for the people who run hosts (the machines an agent operates) and controllers (the
machines the agent runs on). It covers installation, pairing, access control, the checks and logs
you use to see what is going on, and what to do when something is refused or does not work.

> Status: pre-alpha. The Windows host is the M0 target; the macOS host is *(planned)* for M1.
> Commands may still change before the first release.

Related: [architecture.md](architecture.md) for the design, [tools.md](tools.md) for every tool and
the configuration formats.

## Contents

1. [Command overview](#1-command-overview)
2. [Profiles and files](#2-profiles-and-files)
3. [Running the host](#3-running-the-host)
4. [The controller agent](#4-the-controller-agent)
5. [Pairing step by step](#5-pairing-step-by-step)
6. [Levels, revoke, unrevoke, forget](#6-levels-revoke-unrevoke-forget)
7. [When a host refuses](#7-when-a-host-refuses)
8. [Doctor](#8-doctor)
9. [Approvals](#9-approvals)
10. [Audit log](#10-audit-log)
11. [Host configuration file](#11-host-configuration-file)
12. [Connecting across networks](#12-connecting-across-networks)
13. [Troubleshooting](#13-troubleshooting)

## 1. Command overview

Every command takes `--home <DIR>` (another profile, see section 2) and `--json` (machine-readable
output; `host`, `host install`, `host uninstall`, `mcp`, `agent` and `stop-agent` ignore it). Both
options work before or after the subcommand. `ai-remote <command> --help` describes the rest.

**On the host:**

| Command | What it does |
|---|---|
| `ai-remote host [--no-pair-code]` | run the host in this terminal until Ctrl+C or `stop-host` |
| `ai-remote host install` | start the host automatically at logon, in the background (`--service`: as a Windows service that reaches the lock screen) |
| `ai-remote host uninstall` | remove the autostart entry, stop the host, remove the firewall rules |
| `ai-remote host status` | autostart entry, firewall rules, whether the host runs |
| `ai-remote host start` | start the installed background host now (after `stop-host`); the desktop app's tray uses it. A Windows service needs an administrator terminal |
| `ai-remote doctor` | graded checks: can this machine really be operated? |
| `ai-remote pair-code [--level L] [--name N]` | show a one-time pair code (QR code and eight digits) and the ticket behind it |
| `ai-remote peers` | paired controllers with their levels and device ids; revoked ones are marked `REVOKED` |
| `ai-remote level <peer> <observe\|control\|admin\|none>` | change a controller's level |
| `ai-remote revoke <peer>` / `unrevoke <peer>` | refuse a controller, or lift that |
| `ai-remote forget <peer>` | remove a controller's pairing |
| `ai-remote stop-host` | stop the running host |
| `ai-remote bypass [on\|off <peer>\|--all] [--for 2h]` | trust controllers with everything: every tool, no approvals, no deny rules (section 6.1) |
| `ai-remote bypass default on\|off` | trust every paired controller, those paired later included, until you turn one off (section 6.1) |
| `ai-remote approvers [add\|remove]` | approver keys: people who may approve from a paired device ([approvals.md](approvals.md)) |
| `ai-remote mount list\|add\|add-profile\|remove\|test` | manage mounted MCP servers ([tools.md](tools.md#7-mounted-mcp-servers)) |

**On the controller:**

| Command | What it does |
|---|---|
| `ai-remote pair <code\|ticket> [--name N]` | pair with a host by its pair code (`4821 0937`) or its ticket |
| `ai-remote hosts` | paired hosts and the level each granted at pairing; `(default)` marks the default host |
| `ai-remote hosts default [<host>\|--clear]` | show, set or clear the host commands and agents use when they name none |
| `ai-remote network [mode auto\|lan \| discovery on\|off \| upnp on\|off]` | show or change this computer's network settings for its host and controller alike (the desktop app's Settings > Network; section 12) |
| `ai-remote hosts address <host> [<address>]` | show or set where a host is dialled (`ip`, `ip:port`, several separated by commas); how a LAN-mode host whose address changed is reached again (section 12) |
| `ai-remote unpair <host> [--local]` | unpair a host: it drops this device too when it can be reached, then this controller forgets it (`--local`: only here) |
| `ai-remote access [--host H]` | what this device may do on a host: your level, the standing rules that concern you, the paired devices (all of them for an admin) and the tools on offer |
| `ai-remote lower <level> [--host H]` | lower this device's level on a host; raising it again takes someone at that computer (`ai-remote level`) |
| `ai-remote call <tool> ['{json args}'] [--host H]` | call one tool and print the result |
| `ai-remote exec`, `job`, `shot`, `windows`, `info`, `doctor --host`, `ls`, `stat`, `mkdir`, `rm`, `push`, `pull` | operate a host from a terminal or script (section 4.1) |
| `ai-remote watch [--host H]` | follow a host's activity live: recent history, then every call and session as it happens |
| `ai-remote view [--host H] [--seconds N] [--out F]` | watch a host's screen for a while and report the stream (frames, size, rate); `--out` saves the H.264 |
| `ai-remote approvals` / `approve <id>` / `deny <id>` | list, allow or deny calls waiting for approval ([approvals.md](approvals.md)) |
| `ai-remote approver [--create] [--enroll <host>]` | this device's approver key (`--create` makes it), or offer it to a host |
| `ai-remote take-over [--let-watch]` / `hand-back [--note N]` | take the controls from the agent, and give them back (section 9.3) |
| `ai-remote pause` / `resume` | stop the agent on a host until you resume it |
| `ai-remote tell <text>` | leave a message for the agent; it arrives with its next result |
| `ai-remote mcp` | stdio MCP server for an MCP client |
| `ai-remote agents [--disconnect ID]` | the agents (MCP clients) connected to this controller now, with their call counts; `--disconnect` cuts one off (the app's Settings, Agents does the same) |
| `ai-remote agent` / `stop-agent` | run the controller agent in the foreground, or stop it |

**Either side:** `ai-remote status` shows the profile, and whether a host and an agent run for it
(with their device ids).

## 2. Profiles and files

All state lives in one per-user profile directory:

| Platform | Default location |
|---|---|
| Windows | `%LOCALAPPDATA%\ai-remote\data` (local, never roaming, so one device identity is not copied to several machines) |
| macOS | `~/Library/Application Support/ai-remote` |

`--home <DIR>` or the `AI_REMOTE_HOME` environment variable selects another directory. Several
profiles can run side by side on one machine, each with its own identities, host and agent; this is
how tests and local development run a host and a controller on the same computer. For an MCP client,
add the option to the command line it starts: `ai-remote --home D:\profiles\lab mcp`.

```
<profile>/
  host/
    identity.key         the host's device secret key: treat it as a password
    mesh/                transport state: pairings, levels, revocations, open tickets
    ai-remote.toml       host configuration (section 11)
    audit.jsonl          audit log (section 10)
    host.log             background host log, rotated to host.log.1 and host.log.2
    autostart-task.xml   task definition written by `host install` (Windows)
    host-stderr.log      stderr of the LaunchAgent (macOS)
    jobs/                <id>.log and <id>.json of each background job
    mounts/              working directory of mounted servers: <name>.log, <name>.tools.json
  controller/
    identity.key         the controller's device secret key
    mesh/                transport state: pairings
    hosts.json           level each host granted at pairing, and when
    controller.log       log of the agent when it was started on demand
```

Each running daemon listens on a local control endpoint that only the same user can open: on
Windows the named pipes `\\.\pipe\ai-remote-host-<id>` and `\\.\pipe\ai-remote-controller-<id>`,
elsewhere `host/control.sock` and `controller/control.sock`. `<id>` is a hash of the profile path
(16 hex digits), so profiles never collide. The CLI finds the running host or agent through them.

Logging: `AI_REMOTE_LOG` takes a log filter (for example `AI_REMOTE_LOG=debug` or
`info,ar_host=debug`). Commands log to stderr (`ai-remote mcp` keeps stdout for the protocol); the
background host and an agent started on demand log to their files.

## 3. Running the host

The host must run in the logged-in user's interactive desktop session. It is never a service: on
Windows a service runs in session 0, which has no desktop, so screenshots would be black, input
would go nowhere and no approval prompt could be shown.

### 3.1 In a terminal

```sh
ai-remote host
```

It prints its name, device id, profile, number of paired controllers and the audit log path. If no
controller is paired yet, it also shows a pair code with its QR code and ticket (level `control`); `--no-pair-code` skips
that. If approval prompts cannot be shown right now (a locked desktop), it warns that actions which
need approval will be refused.

Stop it with Ctrl+C, or with `ai-remote stop-host` from another terminal. Stopping closes every
controller connection with a "host stopping" reason. Only one host runs per profile: a second
`ai-remote host` fails with "a host is already running for this profile".

### 3.2 In the background: install and autostart

```sh
ai-remote host install --dry-run   # print every change, change nothing
ai-remote host install
```

Repeating `host install` is safe; it is also the upgrade path after you move or update the
binaries. `ai-remote-hostw` must sit next to `ai-remote`: the autostart entry runs it.

#### Windows

Who runs the install:

| Your account | Run it from | Result |
|---|---|---|
| administrator | an elevated terminal ("Run as administrator") | logon task and firewall rules |
| administrator | a normal terminal | refused, with the command to run elevated (the task needs it for "highest privileges", the firewall rules need it anyway) |
| standard user | a normal terminal | logon task only; it prints the command an administrator runs for the firewall rules. The host then runs with the user's own rights, so it cannot operate elevated windows (doctor warns) |
| administrator | `host install --firewall-only`, elevated | only the firewall rules, for the programs of this installation (they apply to every account) |

An elevated terminal that runs as another account than the one using the desktop (a standard
user's terminal elevated with an administrator's credentials) is refused: the task would start the
host when the administrator logs on, with the administrator's profile. The fix is to run
`ai-remote host install` unelevated in the user's own terminal and have an administrator run
`host install --firewall-only`. `--force` installs for the elevated account anyway; the host then
starts at that account's next logon and is not started now.

What the install does (all of it is printed by `--dry-run`, including the full task XML):

1. Writes the task definition to `<profile>/host/autostart-task.xml` and registers it with
   `schtasks.exe` (called by absolute path) as `\ai-remote\host-<8 hex digits>`, one task per
   profile. The task:
   - starts at the logon of your account (by SID, so a rename does not break it);
   - runs with an interactive token in your desktop session, with highest privileges (so the host
     can drive elevated windows);
   - runs `ai-remote-hostw.exe --home "<profile>"`;
   - has no execution time limit (the default stops a task after 3 days), is never stopped or
     blocked on battery, starts late if the logon was missed, and ignores a second start while one
     runs.
2. Firewall (administrator steps): removes earlier `ai-remote host` rules for the two programs at
   their current paths (rules for a folder you moved them out of stay; delete those in `wf.msc`),
   removes stale block rules for the two programs (typically left by an unanswered "allow access?"
   prompt), and adds inbound allow rules named `ai-remote host` for UDP and TCP, any network
   profile, for both `ai-remote-hostw.exe` and `ai-remote.exe`. A block rule for every program's
   inbound UDP is reported but not changed (review it in `wf.msc`).
3. Unless `--no-start`, starts the task, waits up to 20 seconds for the host to answer, and prints its
   pid, its log path and the host's own doctor report as a self-check, then the next step (pairing)
   if nobody is paired. If a host already runs for the profile (in a terminal, or a background host
   started earlier), it is left alone and the new entry takes over at the next logon; to switch now,
   run `ai-remote stop-host` and then `ai-remote host install` again.

If a step fails, the steps after it are not run; fix the cause and run the same command again.

#### Windows: as a service (lock screen, sign-in screen, UAC prompts)

```sh
ai-remote host install --service --dry-run   # elevated terminal; print every change, change nothing
ai-remote host install --service
```

The logon task above runs the host as you, in your desktop session. Windows keeps a *user* process
away from the secure desktop: once the screen locks, a UAC prompt opens, or the machine reboots to
the sign-in screen, that host can neither see nor operate anything until someone unlocks it at the
keyboard. `--service` registers a Windows service instead (RustDesk takes the same route): the
service runs as LocalSystem, starts automatically, and keeps `ai-remote-hostw` running **as SYSTEM
inside the console session**. A SYSTEM process in that session may open the secure desktop, so the
live view (Desktop Duplication is chosen automatically in service mode), screenshots and input
follow the lock screen, UAC prompts and the sign-in screen after a reboot; a controller can type a
password at the lock screen or press `ctrl+alt+del` (the host asks the service to send it, since
the keys themselves are ignored). The service follows the console session across log-offs and
user switches, restarts the host after a crash with the supervisor's back-off, and stops with it
after a clean `ai-remote stop-host`.

**Consequence, read before installing:** that host, and so its `shell`, `jobs` and `files` tools,
run as SYSTEM. Levels, policies, approvals and the audit log are the same code, but what an
admin-level (or bypassed) controller can do grows from "what you can do" to "what SYSTEM can do".
Grant levels accordingly. `--dry-run` prints this with the plan. Approval prompts still cannot be
shown on the lock screen (nobody is there to answer): while it is locked, actions that need
approval are approved from approver devices only (section 9.2).

What the install does (administrator steps, all of them; a standard user is refused):

1. Ends and deletes the logon task of this profile if there is one (both would start a host, and
   the second would find the profile taken).
2. Registers (or updates) the service `ai-remote-host-<8 hex digits>` with `sc.exe` (absolute path):
   LocalSystem, automatic start, running
   `ai-remote-hostw.exe --service --home "<profile>" --owner-sid <your SID>`, restarted by the
   service manager after a crash. The `--owner-sid` is your account: the host's control endpoint
   (an owner-only named pipe) admits it besides SYSTEM, so `ai-remote host status`, `stop-host`,
   `doctor` and the desktop app keep working for you; other accounts are refused as before.
3. Sets the `SoftwareSASGeneration` policy (`HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System`)
   to 1 ("services") when it is not already 1 or 3, so the service may send Ctrl+Alt+Del. The value
   it found is written to `<profile>/host/sas-policy-before.txt`, and `host uninstall` puts it back.
4. The same firewall steps as the logon task install.
5. Unless `--no-start`, starts the service and waits for the host as above.

`ai-remote host status` then shows `Windows service ai-remote-host-… is installed, running`.
`ai-remote host uninstall` stops and deletes the service, restores the policy, and does the task and
firewall steps as before. `stop-host` stops the host *and* the service (a clean exit ends both, like
the supervisor); `ai-remote host install --service` again, or `sc start ai-remote-host-…`, brings it
back. A host started in a terminal while the service runs takes the profile; the service retries
every 30 seconds and takes over once that host stops.

What a locked machine looks like from the controller: `doctor` reports `desktop` as **warn** ("locked
or showing a security prompt …; this host can see and operate it"), `approval` as warn, and the
tools work; the screen shows the lock screen. Without `--service` the same state is a **fail** and
every screen and input tool refuses with "which this host cannot reach".

#### macOS

The install writes a per-user LaunchAgent
`~/Library/LaunchAgents/io.github.jia-ss.ai-remote.host.<8 hex digits>.plist` and loads it with
`launchctl bootstrap gui/<uid>`. It runs `ai-remote-hostw --home <profile>` at login, only in the GUI
(Aqua) session, as an interactive process, and launchd restarts it after a crash but not after a
clean exit, so `ai-remote stop-host` stays stopped until the next login. No elevation is needed.
macOS asks to allow Screen Recording and Accessibility for `ai-remote-hostw` the first time it
captures or acts; grant both in System Settings > Privacy & Security.

**The lock screen** is reachable from this host: while the screen is locked, `doctor` reports
`desktop` as **warn** ("the desktop is locked … (input desktop: lock screen); this host can see and
operate it"), a controller sees the lock screen and can type the password, and actions that need
approval are refused (nobody is there to answer; approver devices still can). The host does not
need `--service` for that on macOS.

**The login window** (after a reboot or a logout) is a different session, which a per-user agent
never enters. `host install --service` writes a *system* LaunchAgent instead,
`/Library/LaunchAgents/io.github.jia-ss.ai-remote.host.<8 hex digits>.plist`, with
`LimitLoadToSessionType` `LoginWindow` and `Aqua` (the way RustDesk reaches the login window), and
removes the per-user one. That file belongs to root, so run it with `sudo`:

```sh
sudo ai-remote --home "$HOME/Library/Application Support/ai-remote" host install --service --dry-run
sudo ai-remote --home "$HOME/Library/Application Support/ai-remote" host install --service
```

`--home` names your profile (sudo changes `HOME`); the plan reads the account from the profile
directory's owner. In the login window's session launchd starts the host as root; the host then
becomes the installing user (`--owner-uid`, written into the plist), so the profile stays that
user's and the Screen Recording and Accessibility grants that user gave `ai-remote-hostw` apply. In
the user's own session the same agent runs as the user, exactly like the per-user one. *Untested on a
real machine yet*: whether ScreenCaptureKit delivers the login window to a process that dropped from
root to the user. If `doctor` says the desktop is usable but the picture stays black there, set
`AI_REMOTE_KEEP_ROOT=1` in the agent's environment (an `EnvironmentVariables` entry in the plist) to
run as root at the login window, as RustDesk does, and report it. `sudo ai-remote … host uninstall`
removes the system agent; without sudo the uninstall stops at it and says so.

**Fast user switching and logout.** launchd runs one copy of the system agent per GUI session:
one in your session, one at the login window. With fast user switching ("Login Window..." in the
user menu) your session keeps running in the background while the login window shows, so both
copies run at once against the same profile. One rule decides which serves: **only the copy whose
session is on the console holds the profile.** The copy that holds it checks every second; after two
reads in a row saying its session left the console it closes the controllers' connections and
exits with code 4 while still holding the profile, which frees the control endpoint and the
transport in the same instant. The other copy, which was polling, claims it within a second and
starts the transport. A controller sees a disconnect and reconnects to the same device. Switching
back works the same way the other round. A real logout ends your session and its copy with it; the
login window's copy then claims. The lock screen is still your session on the console: nothing
changes there. Expected handover: about 2-3 s to notice the switch plus the transport's start
(derived, *not measured on a real machine yet*).

A copy that waits does not exit and does not restart every 10 s; it logs one line per change, for
example:

```text
waiting: this session is not on the console (the screen shows another session or the login window)
waiting: another host holds this profile (the other session's instance, or a host in a terminal)
this session has the console: taking over the profile
stepping aside: this session left the console; the instance that has it takes over
not starting: the host was stopped with `ai-remote stop-host` (see `host status`)
```

Consequences worth knowing:

- A host in a terminal (`ai-remote host`) keeps the profile; the agent waits with one `waiting` line
  and takes over when you quit the terminal host, with Ctrl+C or with `stop-host` alike (only the
  system agent records a stop).
- `ai-remote stop-host` on the system agent stays a stop: the stopping host records it in
  `<profile>/host/stopped`, and the other copy does not take over. A stop made in your session holds
  there (lock screen, switching back) and at the login window until the next real login or a
  restart; a stop made while the login window was served holds in every session until a restart.
  `host install` (and `host uninstall`) clears it, and `host status` says when it holds. If the
  record cannot be written, `stop-host` is refused rather than handing over to the other copy. To
  restart the agent after granting privacy permissions, use `launchctl kickstart -k
  gui/<uid>/<label>` rather than `stop-host`.
- The login window's copy reads its session with the identity it switched to, which is not verified
  on a real machine yet. Each copy logs a `console read` line whenever what it reads changes. When
  that session cannot be read, the login window's copy falls back to the owner of `/dev/console`
  (root while the login window has the screen) and gives the profile back once a user's session
  has the screen, even if launchd keeps it running.
- A transport database that stays unopenable for 15 s (normally it is the previous holder exiting)
  stops the copy with exit 1; launchd starts it again, throttled.
- A session that is not on the console, such as one opened through Screen Sharing, never serves the
  profile: the host there steps aside.
- Another user's session also starts a copy (as that user); it exits at once without touching the
  owner's profile.
- `AI_REMOTE_KEEP_ROOT=1` together with this handover is untested: the login window's copy then
  creates the control lock and the stop record as root, and your session's copy may fail to open
  them.

#### Checking and removing it

```sh
ai-remote host status
```

prints the autostart entry and what it runs (and whether that is this installation and profile), the
inbound firewall rules for each host program (allow protocols, block rules that apply), whether the
firewall is off, and whether the host runs (pid, background or in a terminal).

```sh
ai-remote host uninstall --dry-run
ai-remote host uninstall
```

stops the running host (waits up to 10 seconds), ends and deletes the task (or unloads and removes the
LaunchAgent), removes the task XML, and removes the firewall rules, unless another profile's task
still starts the same `ai-remote-hostw` (the rules are per program, so they stay for it) or the
other tasks cannot be listed (the output then names the `netsh` command to remove them). The same
elevation rules apply: a standard user's uninstall leaves the firewall rules, which an administrator
removes with `ai-remote host uninstall --firewall-only`.

### 3.3 The background host: supervisor, logs, restarts

`ai-remote-hostw` is the same host without a console window. Everything the terminal host would
print goes to `<profile>/host/host.log`, and it never mints a pairing ticket by itself.

- **Supervisor (Windows).** The process the task starts is a small supervisor that runs the host as a
  child in a kill-on-close job object. When the host exits abnormally (crash, panic, error), the
  supervisor starts it again after 5 seconds, doubling the wait up to 5 minutes; a host that ran for
  10 minutes or more counts as healthy and the wait starts over at 5 seconds. A clean stop
  (`ai-remote stop-host`) ends both. Ending the task, or the supervisor dying, ends the host and
  everything in its job. The supervisor's job lets background jobs leave it, but the job object
  Task Scheduler runs every task in does not, so whenever the background host process ends (a stop,
  a crash, a restart), the background jobs it started end with it.
  (Task Scheduler's own "restart on failure" only covers a task that fails to launch, never a
  program that exits with an error.) On macOS launchd's `KeepAlive` does the same job.
- **One host per profile.** A second start (a double-click, a script) finds the profile taken, writes
  "not starting: a host is already running for this profile" to the log and exits with code 3,
  without touching the running host.
- **Logs.** `host.log` is rotated when it passes 8 MiB; two earlier files are kept (`host.log.1`,
  `host.log.2`). Errors before the log can be opened go to `ai-remote-hostw-error.log` in the
  temporary directory. On macOS, launchd also writes the process's stderr to `host/host-stderr.log`.
- **Starting it again** after `stop-host`: `ai-remote host start` (or the desktop app's tray), log off
  and on, run `ai-remote host install` again (it starts the task when no host runs), or start the task
  yourself:
  `schtasks /Run /TN "ai-remote\host-<8 hex digits>"` (`ai-remote host status` shows the name).
- **No console windows.** The background host has no console, so everything it starts (shell
  commands, jobs, mounted servers, system tools) is started without a console window; nothing pops
  up on the user's desktop.

## 4. The controller agent

The controller side is two programs:

- `ai-remote agent` is a per-user daemon. It owns the controller identity and pairings and keeps one
  connection per host; every MCP session is a stream on it.
- `ai-remote mcp` is what an MCP client starts. It connects to the agent (starting it if needed),
  relays stdio to it and exits when the client closes stdin.

The agent starts on demand: every controller command (`pair`, `hosts`, `call`, `exec`, `mcp`, ...)
starts it in the background when it is not running (waiting up to 20 seconds), with its output appended to
`<profile>/controller/controller.log`. It exits after 30 minutes without any client; an open MCP
session counts as a client. `ai-remote stop-agent` stops it now, and `ai-remote agent` runs it in the
foreground for debugging (it fails if one already runs for the profile).

Adding ai-remote to an MCP client:

```sh
claude mcp add ai-remote -- ai-remote mcp
```

Any MCP client that can start a stdio server works the same way: command `ai-remote`, arguments
`["mcp"]` (add `"--home", "<DIR>"` for another profile). Several clients can run at once; they
share the agent and its connections.

For scripts, `ai-remote call <tool> '<json args>' [--host <name>]` calls one tool and prints the
result (images as a one-line summary). It exits with code 2 when the tool reports an error (a
refusal included) and with code 1 when the call could not be made (host unreachable, bad
arguments). `--json` prints the raw result instead and does not use code 2. Each `call` is a new
MCP session, so screenshot ids from an earlier `call` are unknown to the next one.

### 4.1 Operating a host from the command line

`ai-remote call` reaches every tool, but its JSON arguments and raw results are clumsy for daily work.
These commands cover what people and scripts do most, with plain arguments and output. Each one is
one or more tool calls through the agent, so the host's levels, policy, approval and audit apply
exactly as they do to an agent's calls. `--json` prints machine-readable output.

| Command | Tool(s) | Level |
|---|---|---|
| `ai-remote info` | `system_info` | observe |
| `ai-remote doctor --host H` | `doctor` | observe |
| `ai-remote shot [-o F.png] [--monitor N] [--max-long-edge PX]` | `screenshot` | observe |
| `ai-remote windows [--filter T] [--all]` | `list_windows` | observe |
| `ai-remote ps [<name>] [--pid N] [--limit N]` | `process_list` | admin |
| `ai-remote kill --pid N \| --name X [--no-tree] [--intent WHY]` | `process_kill` | admin, asks |
| `ai-remote exec "<command>" [--shell S] [--cwd D] [--timeout S] [--env K=V] [--stdin F]` | `shell` | admin, asks |
| `ai-remote job start "<command>"` / `list` / `status <id>` / `log <id> [-f]` / `wait <id> [--timeout S]` / `kill <id>` | `jobs_*` | admin; `start` asks |
| `ai-remote ls <dir> [--pattern P]` / `stat <path>` | `files_list` / `files_stat` | admin |
| `ai-remote mkdir <dir>` / `rm <path> [-r]` | `files_mkdir` / `files_delete` | admin, asks |
| `ai-remote push <local> <remote> [--force] [--mkdir]` | `files_push` + the file channel | admin, asks |
| `ai-remote pull <remote> [<local>] [--force]` | `files_pull` + the file channel | admin, asks |

**Which host.** `--host <name>`, else the environment variable `AI_REMOTE_HOST`, else this device's
default host, else the only paired host. `ai-remote hosts default <name>` sets the default (kept in
`controller/hosts.json`; MCP sessions use it too when a call names no host and `use_host` was not
called), `ai-remote hosts default --clear` removes it, and `ai-remote hosts` marks it.
`AI_REMOTE_HOST` applies to every controller command that takes `--host`, but not to `doctor`, which
checks this machine unless `--host` is given.

**Exit codes.** 0: done. 2: the host refused the call or the tool failed (the host's message is on
stderr). 1: the call could not be made (no agent, host unreachable, bad arguments). `exec` exits with
the remote command's own exit code instead, like ssh: 124 when it timed out and was killed, 255 when
it did not run at all. `job wait` exits with the job's exit code (124: still running at `--timeout`;
137: killed; 255: unknown, such as a job the host re-adopted after a restart).

**Files.** File commands take absolute paths on the host. `push` hashes the local file, asks the host
to approve exactly that size and SHA-256, then streams it; the host writes a temporary file next to
the target and moves it into place only when size and hash match, so a broken transfer never leaves
a half-written target. A remote path that ends with `/` or `\`, or names a folder that exists, gets
the file under its own name; `--mkdir` creates missing folders and `--force` replaces an existing
file. `pull` streams into a local temporary file and keeps it only when the host's SHA-256 matches;
it refuses to replace a local file without `--force` before it asks the host anything. One file per
transfer: pack a folder first (`tar -czf`). Every transfer is in the audit log (`transfer` lines with
path, bytes, duration and outcome).

**Approval.** `exec`, `job start`, `kill`, `mkdir`, `rm`, `push` and `pull` ask the owner by default, as an
agent's calls would. For a test machine you drive from your own controller, a rule on that host
spares the prompt for your device only:

```toml
[[rule]]
tool = "files_*"
principal = "my-laptop"   # this controller's name in `ai-remote peers` on the host
action = "allow"
```

For example:

```sh
ai-remote hosts default lab-pc
ai-remote info
ai-remote exec "Get-Process game -ErrorAction SilentlyContinue | Select-Object Id, WS"
ai-remote push build.zip 'D:\deploy\' --mkdir
ai-remote exec "Expand-Archive D:\deploy\build.zip D:\deploy\build -Force" --timeout 300
ai-remote job start "D:\deploy\build\game.exe" --intent "performance baseline"
ai-remote job log <id> -f
ai-remote shot -o lab.png
ai-remote pull 'D:\captures\frame.ngfx-gputrace' traces/
```

## 5. Pairing step by step

1. **Make sure the host runs** (`ai-remote host status` or `ai-remote status` on the host).
2. **Show a pair code on the host** (desktop app: This device → Show pair code; tray: Show Pairing
   Code), or in a terminal:

   ```sh
   ai-remote pair-code --level control --name laptop
   ```

   `--level` is `observe`, `control` (default) or `admin`: start with the lowest level that does the
   job. `--name` is the host's own name for the controller that pairs (default: the name the
   controller suggests, its machine name). It shows a QR code, an eight-digit pair code and the
   ticket behind them. The code works once, for 10 minutes, and only while the host runs; showing a
   new code withdraws the old one. Five connections that fail to prove the code burn it.
3. **Give the code to the other device** in person, or on a channel you trust. It is a credential
   while it lasts: whoever uses it first gets the level.
4. **Pair on the controller:** scan the QR code with the iPhone app, type the digits on the desktop
   app's Pair a computer page, or:

   ```sh
   ai-remote pair 4821 0937 --name office-pc
   ```

   `--name` is your local name for the host (default: the name the host suggests). It prints the
   host's name, a three-word safety code and the granted level. The controller finds the host behind
   the code through the same discovery the connection uses (n0's DNS/pkarr service, mDNS on the
   local network, the relays). If the two machines cannot reach each other that way (no internet and
   no shared network), use the ticket instead: `ai-remote pair <ticket>`, valid once for 24 hours.
5. **Compare the safety code** with what the host shows:
   - a host in a terminal prints `Paired with controller "laptop" (level: control).` and
     `Safety code: ...`;
   - a background host writes a `controller paired` line with `peer=`, `level=` and `sas=` to its
     log. On Windows:

     ```powershell
     Select-String -Path "$env:LOCALAPPDATA\ai-remote\data\host\host.log" -Pattern 'controller paired' |
       Select-Object -Last 1
     ```

6. **If the words differ**, someone else redeemed the ticket. On the host run
   `ai-remote forget <name>` (or `revoke` to refuse that device for good), on the controller
   `ai-remote unpair <host>`, and start again with a new ticket.
7. **Check the result:** `ai-remote hosts` on the controller, `ai-remote peers` on the host, and
   `ai-remote call system_info` (add `--host office-pc` when several hosts are paired).

Names are lower-cased, every character other than an ASCII letter or digit becomes a dash, runs of
dashes become one, dashes at either end are dropped, and the result is cut to 40 characters
(`Office PC (2)` becomes `office-pc-2`). Commands that take a peer or host accept its name
or its `eid:` device id; `level`, `revoke`, `unrevoke`, `forget`, `unpair` and the MCP `host` argument
also accept a unique prefix of either.

## 6. Levels, revoke, unrevoke, forget

| Level | The controller can use |
|---|---|
| `observe` | `system_info`, `doctor`, `screenshot`, `zoom`, `list_windows`, observe tools of mounts |
| `control` | + `focus_window`, `mouse`, `keyboard`, `clipboard_read`, `clipboard_write`, control tools of mounts |
| `admin` | + `shell`, `jobs_*`, `files_*`, admin tools of mounts |

A controller only sees the tools its level allows. All four commands below run on the host and take
effect immediately; changes to `ai-remote.toml` (policy rules, mounts, approval timeout, relays,
name) only apply when the host restarts.

| Command | Pairing | Live connections | What the controller hears | Undo |
|---|---|---|---|---|
| `level <peer> <level>` | kept | kept; the next call is checked against the new level | a call above the level: "'shell' requires level 'admin', but this device has 'control' ..." | `level` again |
| `level <peer> none` | kept | kept, but every call is refused | "This device no longer has access to this host."; new connections: "no access level" | `level <peer> <level>` |
| `revoke <peer>` | kept, marked `REVOKED` in `peers` | cut | "access revoked by the host ..." (also on later attempts while its agent runs) | `unrevoke <peer>` |
| `unrevoke <peer>` | unchanged | - | the device can connect again if it is still paired | `revoke` |
| `forget <peer>` | removed (with its level) | cut | "not paired: ... Pair again with a fresh ticket" | a new ticket |

Details:

- **Revoke** refuses the device even if it redeems a new ticket. The controller agent remembers that
  it was revoked, because later attempts only meet the transport's generic refusal. It keeps that in
  memory only: once the agent has restarted (it exits after 30 idle minutes), the same refusal reads
  `not paired`.
- **Forget** a revoked device: the pairing goes, the refusal stays. The CLI prints the device id to use
  later: `ai-remote unrevoke eid:<id>`. After that the device still needs a new ticket.
- **Unrevoke** takes the peer name while the pairing exists, otherwise the device id (or a unique
  prefix of it). The controller's refusal messages include its device id for this purpose.
- **Unpair** on the controller only forgets the host there; the host still lists the controller until
  you `forget` it on the host too.
- MCP clients usually cache the tool list. After a level change, reconnect the client (or start a
  new session) to see the tools of the new level; calls are checked against the current level either
  way.

### 6.1 Bypass: trusting a controller with everything

For a computer you only operate from your own devices (a test machine, say), approvals and levels can
be in the way. **Bypass** lets you trust chosen controllers with everything:

```sh
ai-remote bypass                       # each paired controller, BYPASS where it is on
ai-remote bypass on my-laptop          # until you turn it off
ai-remote bypass on --all --for 2h     # every controller that can connect, ending by itself after 2 hours
ai-remote bypass off my-laptop         # or --all
ai-remote bypass default on            # every paired controller, including ones you pair later
ai-remote bypass default off
```

**The default.** A computer that only you (and your agents) ever operate can trust every controller
up front: `ai-remote bypass default on` covers every paired controller, those you pair later
included, with no per-device step. When something looks wrong, `ai-remote bypass off <peer>` takes
that one controller out of the default (it is listed as *turned off* in `ai-remote bypass`), as do
`level <peer> none`, `revoke` and the controller lowering its own level; `bypass on <peer>` puts it
back. `forget` clears everything about a device, so pairing it again starts from the default.
`bypass default off` goes back to trusting only the controllers you turned on one by one. The
default is kept in the same file, is audited (`kind: "bypass"` with `default`), and `doctor` warns
while it is on.

The desktop app on that computer does the same in **Settings, This device, Controllers trusted with
everything**: each paired controller is either *Asks first* (the default) or *Everything allowed*,
and choosing the latter asks you to confirm.

While a controller is bypassed, its calls (and those of every agent working through it):

- are checked as `admin`, whatever its level, so it sees and can use every tool, file transfers
  included;
- never wait for approval, and are not stopped by `deny` rules (or `approve = "deny"` on a mounted
  tool);
- are still audited, with `"bypass": true` on their records.

An MCP client that cached its tool list sees the extra tools after it reconnects, as after a level
change.

What still holds: the device needs its pairing and some level (`level <peer> none`, `revoke` and
`forget` cut it off, and also end its bypass, as does a controller lowering its own level); a person
can still take over or pause the agent (section 9.3). The controller can see that it is bypassed
(`ai-remote access`, `ai-remote info`, `system_info.you.bypass`; the desktop app marks the computer
Everything allowed in its header and Access tab, the iPhone app in the session's permission label,
which also enables taking over whatever the level), and
`ai-remote doctor` warns while any controller is bypassed.

Bypass is set only at this computer, over the host's local control channel; no controller can ask
for it, so an agent cannot give it to itself through ai-remote. It is kept in
`<profile>/host/bypass.json` and survives restarts; every change is an audit line (`kind:
"bypass"`). An agent that already runs `shell` on the host as its user could still change that
file, as it could any of the host's configuration (see the threat model in
[approvals.md](approvals.md)).

## 7. When a host refuses

Refusals are written for both the agent and you: each says what happened and what to do. An agent is
told not to retry refused or declined calls automatically.

### 7.1 Connection refusals

What the controller reports when it cannot get a session with a host (messages are shortened here):

| Message starts with | Meaning | What to do |
|---|---|---|
| `access revoked by the host` | the host owner revoked this controller | on the host: `ai-remote unrevoke <this controller>`; if the pairing was removed too, pair again |
| `not paired` | the host does not know this controller (never paired, forgotten, or revoked before this agent started) | pair again with a new ticket; if it was revoked, the owner also has to `unrevoke` it |
| `no access level` | paired, but the host grants no level | on the host: `ai-remote level <this controller> control` |
| `host offline or unreachable` | no answer; the raw cause follows | start the host, check the machine is online, see section 13 |
| `the host closed the connection` | the host stopped or restarted | try again shortly |
| `this controller closed the connection` | the host was unpaired here, or the agent is stopping | pair again, or retry |
| `this controller has revoked that host` | this machine refuses that host | - |
| `connection failed:` | anything else, with the raw reason | read the reason; check versions match |

When the host owner has to act, the message ends with `This controller's device id: eid:...`, which
is how the owner finds it in `ai-remote peers` (or uses it with `unrevoke`).

A refusal that only a person can lift is reported once and not redialled in a loop; the next attempt
dials again, so a fix on the host is picked up.

If the connection breaks while a call runs, the call is not repeated. The message ends with "The
call's outcome is unknown: it may have run on the host. Check the host's state before repeating it".

### 7.2 Call refusals by the gate

| Message | Cause | What to do |
|---|---|---|
| `'<tool>' requires level '<x>', but this device has '<y>'` | the tool needs a higher level | the owner raises it with `ai-remote level`, if appropriate |
| `This device no longer has access to this host.` | the level was set to `none` (or the pairing went) | the owner grants a level again |
| `The host's policy does not allow '<tool>'.` | a `deny` rule or `approve = "deny"` | the owner changes `ai-remote.toml` |
| `The host owner declined '<tool>'.` | the owner clicked No | ask the user what they want |
| `Nobody approved it on the host in time for '<tool>'.` | no answer before the timeout | ask the user to be at the host, or add a rule |
| `No one can approve actions on the host right now (desktop locked or unattended), so it refused '<tool>'.` | no prompt could be shown | unlock the desktop, or add a rule (section 9) |
| `cancelled: the session ended` / `cancelled by the controller` | the session ended or the client cancelled | - |
| `unknown tool '<name>'` | the host does not have that tool (a protocol error, not audited) | check the tool list of that host |

### 7.3 Common tool refusals

| Message contains | Cause | What to do |
|---|---|---|
| `the desktop is locked or showing a security prompt, which this host cannot reach` | locked screen, sign-in screen, UAC prompt, and the host runs as the user | unlock; or install the host as a service (`host install --service`, section 3.2), which reaches the secure desktop |
| `runs in the services session (session 0)` | the host was started in session 0 by hand (the service's own host runs in the console session) | run it in the user's session (`host install`, with or without `--service`) |
| `an ai-remote approval prompt is open on the host` | input is paused while a prompt waits for its answer | wait for the owner to answer, then retry |
| `the input would go to one of ai-remote's own windows` | the target is the approval prompt or the host's terminal | act on another window |
| `running with higher privileges` | the target window belongs to an elevated program | run the host elevated (`host install` as an administrator) |
| `is refused: it locks the workstation` (and similar) | a lock or sign-out chord | do not send it; the owner decides |
| `is refused: it opens the secure attention screen` | `ctrl+alt+del` on a host that does not run as a service | install the host as a service (`host install --service`); it then sends it through the service |
| `mount '<m>' is not running: it failed N times in a row` | a mounted server keeps crashing | read `host/mounts/<m>.log`; see [tools.md](tools.md#75-lifecycle-crashes-and-restarts) |
| `refusing to start ...: its SHA-256 is ...` | the mounted server's executable changed | re-add the mount if the update was intended |

Tool-specific errors are listed with each tool in [tools.md](tools.md).

## 8. Doctor

```sh
ai-remote doctor [--json]
```

asks the running host for its checks, so they see the host's own session, token and network. Without
a running host the CLI runs them itself from your terminal and skips the network check. The same
report is the `doctor` tool (agents are told to run it before a task and stop on `fail`) and the
self-check printed by `host install`.

Each check is `ok`, `warn`, `fail` or `skip` (not applicable, or not checkable from where it ran),
with a fix for anything not `ok`. The overall grade is the worst one; `ai-remote doctor` exits with
code 1 when it is `fail`.

```
Checked by the running host:
  ok    session    interactive session 1
  ok    console    attached to the physical console
  ok    desktop    desktop unlocked (input desktop: Default)
  warn  elevation  not elevated: windows of elevated programs (installers, admin tools) silently ignore its input and UI actions
                   fix: run the host with highest privileges; `ai-remote host install` registers it that way
  ok    dpi        per-monitor DPI aware (v2): screenshot and click coordinates agree
  ok    network    quiet: no local-network discovery (mDNS), no router port mapping, name lookups only through the system's DNS
  ok    relay      connected to https://...; 3 direct address(es)
  ok    p2p        UDP gets through, so direct (P2P) connections can form; IPv6 works
  ok    approval   approval prompts can be shown on this desktop
  ok    firewall   inbound connections allowed for C:\...\ai-remote-hostw.exe (UDP, TCP)
  ok    autostart  Task Scheduler task \ai-remote\host-1a2b3c4d starts the background host at login
  ok    audit      audit log writable: C:\...\host\audit.jsonl
  ok    disk       212.4 GiB free on C:\
Overall: warn
```

| Check | ok when | Not ok means | Fix |
|---|---|---|---|
| `session` | an interactive Windows session (not 0) | **fail**: the services session has no desktop; screenshots are black, input goes nowhere | start the host from the user's session; `host install` registers it that way (the service too: its host runs in the console session) |
| `console` | the session is attached to the physical console | **warn**: a remote desktop session; capture and input stop when it disconnects, and the console stays locked | return the session to the console instead of disconnecting: `tscon <session> /dest:console` from an elevated prompt |
| `desktop` | the input desktop is `Default` (unlocked) | **fail**: locked, or a sign-in or UAC prompt is showing, and the host cannot reach it; **warn**: the same, but the host runs as a service and can see and operate it (approval prompts cannot be shown) | unlock; on unattended machines turn off automatic locking, or install as a service (`host install --service`) |
| `elevation` | the host runs elevated | **warn**: windows of elevated programs ignore its input | run with highest privileges (`host install` as an administrator) |
| `dpi` | per-monitor DPI aware (v2) | **fail**: clicks on scaled displays land in the wrong place | remove any high-DPI compatibility override on the executable (Properties > Compatibility) |
| `network` | `ai-remote.toml` reads; says what the host puts on the network besides its connections (local-network discovery, router port mapping) | **warn**: the file cannot be read | fix the file (section 11) |
| `p2p` | UDP gets out, the NAT is not symmetric | **warn**: UDP blocked (every connection is relayed), a symmetric NAT (direct connections often fail), or a captive portal | allow outbound UDP, turn on router port mapping (Settings > Network) where the router allows UPnP or NAT-PMP, use IPv6, or sign in to the network |
| `relay` | connected to a home relay | **warn**: no relay in use, or none reachable; controllers on other networks may not get through | check internet access, outbound firewall or proxy (relays use HTTPS), or `[network] relays` |
| `lan` | (LAN mode, in place of `relay`; `p2p` is skipped) the host listens on its fixed port; lists the addresses controllers reach it at | **warn**: the fixed port was taken, so the host listens on another one and controllers that stored the fixed port cannot reach it | free the port, or set another `[network] port`, and restart the host |
| `approval` | prompts can be shown | **warn**: actions that need approval are refused | unlock the desktop, or allow trusted tools with `[[rule]]` entries |
| `bypass` | no controller is bypassed | **warn**: the named controllers, or every paired controller when the default is on, use every tool without approval (section 6.1) | `ai-remote bypass off <peer>` (or `--all`, or `bypass default off`) when you no longer need it |
| `firewall` | an inbound allow rule covers UDP for the host program, or the firewall is off | **fail**: a block rule applies to the program (traffic falls back to the relay); **warn**: no allow rule (Windows will prompt at the first connection), or a block rule for every program's inbound UDP | `host install` from an elevated terminal; review rules of other programs in `wf.msc` |
| `autostart` | the task (or LaunchAgent) starts this installation's background host with this profile | **warn**: none installed, or it runs something else | run `ai-remote host install` (again) |
| `audit` | the audit log can be opened for appending | **fail**: actions are not recorded | fix the profile directory's permissions, or use another `--home` |
| `disk` | 5 GiB or more free where the profile is | **warn** under 5 GiB; **fail** under 0.5 GiB (logs and the audit log may stop being written) | free some disk space |

The firewall check looks at the program that runs the host: the running host's executable, or
without a host the one autostart would run.

## 9. Approvals

A call needs approval when its action is `ask`: the default for `shell`, `jobs_start`, `files_mkdir`,
`files_delete`, `files_push` and `files_pull`, for admin tools of mounted servers without an `approve`
setting and for mounted tools with `approve = "ask"`, or whenever a policy rule says `ask` (see
[tools.md](tools.md#8-policy-rules)). Calls of a bypassed controller never ask (section 6.1).

Two kinds of people can answer, and the first answer wins: someone at the host, in a dialog on its
desktop (9.1), and the owner on a paired device whose approver key the host accepted (9.2).

### 9.1 At the host

Unless `approval_prompt = false`, the host shows a native warning dialog on its desktop, titled
"ai-remote: approve an action":

```
"laptop" wants to run: shell

Why: Check free disk space before the update

Details:
{"command":"Get-PSDrive C","intent":"Check free disk space before the update"}

Allow this once? (No answer within 60 s counts as No.)
```

- **Yes** runs this one call. There is no "always allow" button; use a policy rule for that.
- **No**, or no answer within `approval_timeout_secs` (default 60, at least 5), refuses the call, and
  the agent is told which of these happened. When the dialog cannot be shown (locked desktop, secure
  desktop, no interactive session), only approver devices can answer; without any, the call is refused
  at once. After a timeout, or when a device answered first, the dialog may stay on screen, but its
  answer is ignored.
- The text the caller controls is shown safely: control characters are escaped, the intent is cut at
  600 characters and the arguments preview at about 400. Secret values (environment values, standard
  input, clipboard text) appear only by size, such as `<hidden: 12 chars>`.
- While any prompt waits for its answer, every mouse and keyboard call from every controller is
  refused, and input can never be aimed at the host's own windows, so a controller cannot answer its
  own prompt.
- On an unattended machine nobody can answer at the host. Approve from a device (9.2), give such
  devices only the tools they need, or allow specific tools for a specific device with a rule
  (`action = "allow"` skips the prompt).

### 9.2 From a paired device

The owner can allow or deny from a paired device. Allowing needs that device's approver key, which the
OS keeps behind Windows Hello (Touch ID and Face ID with the macOS and iPhone apps), so an agent running
on the same device cannot approve its own request. Denying needs no key. Set it up once:

```bash
# on the controller device: create the key (Windows Hello asks you) and offer it to the host
ai-remote approver --enroll office-pc
```

Someone at the host confirms the new key in a prompt. Without a usable desktop there, print the key
with `ai-remote approver --create` on the device and add it in the host's terminal:
`ai-remote approvers add <device> <scheme>:<key>`. Then:

```bash
ai-remote approvals                                  # what is waiting, with ids
ai-remote approve 3f9c2a                             # Windows Hello, then the call runs
ai-remote deny 3f9c2a --note "use the recycle bin"   # the agent receives the note
```

A machine nobody sits at can set `approval_prompt = false` and a longer `approval_timeout_secs`. The
design, the signed message and the threat model are in [approvals.md](approvals.md).

### 9.3 Taking over, pausing, and messages to the agent

An agent and a person take turns on one computer. From a paired device with level `control` or above:

```bash
ai-remote take-over             # you drive; active/queued agent calls are cancelled, new calls refused
ai-remote take-over --let-watch # same, but the agent may keep looking (observe-level tools only)
ai-remote hand-back --note "I fixed the chart title"
ai-remote pause                 # nobody drives; any paired device may pause
ai-remote resume
ai-remote tell "Use the D: drive for the export"
```

- While you drive, only your own calls run: those the desktop app makes for you, or
  `ai-remote call ... --person`. What you type is logged by size only.
- In the desktop app your mouse and keyboard go over a live input stream (`input/1`) after you take
  over: moves, presses, releases, wheel notches and keys by name, as they happen; the audit log keeps
  the stream's start, end and refusals (`input_start`, `input_end` with event counts and the reason
  it ended, `input_refused`). Only the device that has taken over may open it. It is checked again
  every second: handing back, a pause, another device taking over, a lower level, a bypass that ended
  or a policy change ends it within that second, after the host lets go of every key and button it
  holds; so does three seconds without a word from the controller (the app sends a heartbeat every
  second). When the host's policy says `deny` or `ask` for `mouse` or `keyboard` the stream is refused
  and the app falls back to one tool call per action (which does ask the owner); older hosts get the
  same fallback. On the live stream Cmd on a Mac driving a Windows host acts as Ctrl and Ctrl on a PC
  driving a Mac acts as Cmd (the one-call-per-action fallback still sends Cmd as the Windows key);
  with Ctrl, Option/Alt or Cmd held, letters and digits are sent by key position, whatever the local
  layout; characters the host's layout cannot type are entered as text; lock and sign-out chords are
  refused as for the tools.
- Clipboard text follows both ways while you drive, changes only (taking over replaces neither
  clipboard): text copied on the host lands here, text copied here lands on the host (the app reads
  its clipboard once a second while it is in front, and when you switch back to it; at most 1 MB).
  Reading and writing on the host follow its `clipboard_read` / `clipboard_write` policy (`allow`
  only), and the host's clipboard is left alone while it is locked or on a secure desktop. Files on
  the host's clipboard show a notice at the top of the session with a button that saves them to your
  Downloads folder; files on your clipboard can be sent to the host's desktop; a file dropped on the
  picture (no takeover needed) goes to the host's desktop (`~/Desktop`: the desktop the signed-in user
  sees), never replacing a file of the same name (a `-1` suffix and so on is added). These transfers
  use the same tickets, policy and approvals as `push` / `pull`.
- Taking over or pausing invalidates the grant held by existing agent calls, including pending
  approvals and queued input. Handback/resume allows new calls; it never resumes old ones. Input
  workers check cancellation before every event, including pointer movement, and release held
  keys/buttons when stopping. An event already sent to the OS cannot be undone: inspect the screen
  before issuing a replacement action. `--let-watch` preserves observe-level calls during takeover.
- Input permits one executing call and one waiter. Further calls fail with `input busy`; wait for
  the outstanding calls and inspect the host before sending more. Waiting uses an async lock rather
  than occupying native worker threads needed by capture and approval dialogs.
- Handing back, and every `tell`, queues a message that is added to the agent's next call result
  ("Messages from the owner while you worked: ..."), so the agent knows the screen may have changed.
- Each change is an audit line (`driver`: `agent`, `person` or `paused`, who, and the note), and
  every session records whether it is an agent's or a person's (`client` on `session_start`).
- Turns coordinate people and agents; they are not a security boundary (an agent on the same device
  could make calls marked as a person's). Levels, policy and approvals are what limit an agent.

**Pausing at the host itself.** The owner can also pause from the desktop app on the host (the tray
menu or Settings > This device). That pause is stronger than a controller's: the agent's calls *and* a
person's who took over are refused, and no paired device can resume or take over until the owner
resumes at that computer (`ai-remote resume` from a controller is refused with that reason); live
views keep running. On resume, whoever drove before gets the controls back. The audit log records it
as a `driver` line with `"origin": "owner"`, and controllers' timelines show "someone at <host>
paused it".

## 10. Audit log

Every tool call on the host is appended to `<profile>/host/audit.jsonl`, one JSON object per line.
A call that runs gets two records: `decision` before it runs and `outcome` after it. A refused call
gets only the `decision` record.

| Field | Meaning |
|---|---|
| `ts` | milliseconds since the Unix epoch |
| `kind` | `decision` or `outcome` |
| `session` | 8-hex-digit id of the MCP session |
| `peer` | the host's name for the controller |
| `principal` | the controller's device id, `eid:<hex>` |
| `tool` | tool name (mounted tools with their prefix) |
| `decision` | `allow`, `approved`, `denied`, `timeout`, `unavailable` (no prompt possible), `level` (refused by level), `policy` (refused by a rule) |
| `intent` | the caller's stated reason, when given |
| `args_hash` | BLAKE3 of the argument JSON: matches identical calls without storing values |
| `args_preview` | `decision` records only: the arguments as shown to the owner, at most about 400 characters (head and tail), secrets by size only |
| `outcome` | `ok`, `error` or `cancelled` |
| `duration_ms` | how long the call ran |
| `result_bytes` | size of the result's content as JSON, after the result budget |
| `error` | the error text; over 300 characters it is cut to its first and last 140 |
| `bypass` | `true` when the caller is a bypassed controller (section 6.1): checked as admin, no policy, no approval |

```json
{"ts":1791021600123,"kind":"decision","session":"5f2c19ab","peer":"laptop","principal":"eid:3b7e...","tool":"shell","decision":"approved","intent":"Check free disk space","args_hash":"9f2d...","args_preview":"{\"command\":\"Get-PSDrive C\",\"intent\":\"Check free disk space\"}"}
{"ts":1791021601412,"kind":"outcome","session":"5f2c19ab","peer":"laptop","principal":"eid:3b7e...","tool":"shell","intent":"Check free disk space","args_hash":"9f2d...","outcome":"ok","duration_ms":1289,"result_bytes":512}
```

ai-remote does not rotate or prune the audit log; archive it yourself if it grows. The doctor `audit`
check fails when it cannot be written. Background jobs keep their own records in `host/jobs/`
(command, owner, intent, times, exit code; never the environment).

## 11. Host configuration file

`<profile>/host/ai-remote.toml` is optional; without it every default applies. The host reads it when
it starts, so restart the host after editing it (`ai-remote stop-host`, then start it again). A file
that does not parse stops the host from starting; the error is in the terminal, or in `host.log` for
the background host (on Windows its supervisor keeps retrying with the backoff of section 3.3, so a
fixed file is picked up within at most 5 minutes).

```toml
[host]
name = "office-pc"           # shown to controllers; default: the machine name
approval_timeout_secs = 60   # how long an approval waits; at least 5
approval_prompt = true       # false: no dialog here, approve only from devices (section 9.2)

[network]
relays = []                  # empty: the default public relays (section 12)
relay_fallback_n0 = true     # with relays set: keep n0's public relays as a fallback
proxy = ""                   # empty: detect the HTTP proxy; "none": never use one; or a URL
lan_discovery = "off"        # "on", "resolve" (find peers, do not announce) or "off" (section 12)
port_mapping = false         # ask the router for a port (UPnP); the app's Settings > Network writes it
quiet = false                # true: the two lines above stay off whatever they say (section 12)
mode = "auto"                # "lan": direct on the local network only, no relay (section 12)

[[rule]]                     # policy rules, first match wins
tool = "shell"
action = "ask"

[[mount]]                    # mounted MCP servers
name = "ui"
command = "C:/tools/cua-driver/cua-driver.exe"
args = ["mcp"]
[mount.tools]
click = "control"
```

`[[rule]]` is described in [tools.md](tools.md#8-policy-rules), `[[mount]]` in
[tools.md](tools.md#7-mounted-mcp-servers). Only the `[[mount]]` tables and their tool entries reject
unknown keys; a misspelled key elsewhere (for example `principle` instead of `principal` in a rule) is
ignored without a warning, so check the spelling. `ai-remote mount ...` edits the file and keeps your
comments and layout.

## 12. Connecting across networks

Paired devices find and reach each other without port forwarding or a VPN. The ways below are on by
default, except the two that put traffic on the network beyond the connections themselves: router port
mapping and local-network discovery are off until you turn them on.

| Way | What it does | Default |
|---|---|---|
| Direct (P2P) | UDP hole punching, IPv4 and IPv6 | on |
| Router port mapping | Asks the router to open a port (UPnP, NAT-PMP, PCP), for routers that hole punching alone does not get through | **off**; turn on in the desktop app (Settings > Network) or `port_mapping = true` |
| Local network | Finds the other device on the same network by mDNS, even with no internet | **off**; `lan_discovery = "on"` |
| Local network only | No relay and no address lookup: devices connect directly at stored addresses, and nothing goes on the network but the connections | **off**; Settings > Network in the desktop app, or `mode = "lan"` |
| Relay | When no direct path works, an encrypted relay forwards the traffic over HTTPS (port 443); the connection moves to a direct path as soon as one opens | on |
| HTTP proxy | On networks that only allow traffic out through a proxy, the relay connection goes through it | on; `proxy = "none"` |
| Relay fallback | With your own relays set, n0's public relays stay as a fallback | on; `relay_fallback_n0 = false` |
| Firewall check | The controller notices when the system firewall blocks it (a dismissed "allow access?" prompt leaves block rules) and the desktop app offers "Allow in firewall" (one administrator prompt) | on |
| Retry | The controller dials an unreachable host again after 1 s and 3 s (a network that is just switching often gets through) | on |

Relayed traffic stays encrypted end to end, but adds latency; n0's free public relays are also
rate-limited. Every client says so while it lasts ("P2P failed, relayed by n0's public relay").

**Configuration.** The host reads `[network]` in `<profile>/host/ai-remote.toml` (section 11); the
controller reads the same `[network]` section from `<profile>/controller/ai-remote.toml`. Both are read
when the process starts.

```toml
[network]
relays = ["https://relay1.example.org", "https://relay2.example.org"]
relay_fallback_n0 = true
proxy = ""            # empty or absent: detect; "none": never; or e.g. "http://proxy.corp:3128"
lan_discovery = "off" # "on": announce this device on the local network; "resolve": find others only
port_mapping = false  # ask the router for a port; the desktop app's Settings > Network writes it
port = 0              # a fixed UDP port to forward on your router; 0 or absent: random
quiet = false         # true: lan_discovery and port_mapping stay off whatever they say
mode = "auto"         # "lan": direct on the local network only (no relay, no address lookup)
address_lookup = []   # empty: n0's address lookup service; or your own pkarr servers
```

- **Relays.** An empty list means n0's public relays. With your own
  [iroh-relay](https://github.com/n0-computer/iroh) servers listed, n0's relays are kept as a fallback
  unless `relay_fallback_n0 = false`. A pairing ticket records the relay the host used when it was
  minted, so mint new tickets after changing relays. `ai-remote doctor` on the host shows whether it is
  connected to a relay (`relay` check).
- **In the app.** Settings > Network shows the choice between **Automatic** (recommended: works from
  anywhere) and **Local network only**, and two switches, both off: **Find devices on your local
  network** (`lan_discovery`) and **Ask the router to open a port (UPnP)** (`port_mapping`, hidden with
  Local network only, where it has no use). With Local network only it also shows this computer's address
  (with a copy button) and the address of every computer this device connects to (with a way to change
  it). A change is written to this computer's host and controller configs; the controller applies it at
  once (live sessions reconnect), a host running here when it next starts. `ai-remote network` does the
  same from a terminal.
- **Proxy.** Detected in this order: the `proxy` setting, the `HTTPS_PROXY` / `ALL_PROXY` /
  `HTTP_PROXY` environment variables, the system setting (Windows: Internet Settings of the signed-in
  user; macOS: `scutil --proxy`). Proxy auto-config (PAC) scripts are not evaluated; set `proxy`
  explicitly there. Only HTTP(S) proxies work (not SOCKS). Only the relay connection uses the proxy:
  direct paths are UDP, and the address lookup of a peer whose address changed still goes to n0's DNS
  service directly. The host log names the proxy in use.
- **Local network.** Off by default. `"on"` announces this device on the local network (its device id and
  addresses, including its public address, about once a second, to every device there) so others can find
  it with no internet at all; `"resolve"` finds others without announcing, but still sends a query about
  once a second. With internet, devices on the same network connect directly without it: the addresses they
  publish include the local ones. With Local network only, turning it on means computers whose IP changed
  are found again without setting their address by hand. The app's switch writes `"on"` or `"off"`.
- **Router port mapping.** Off by default. Turn it on in the desktop app (Settings > Network): it writes
  `port_mapping = true` to this computer's host and controller configs; the controller applies it at once
  (live sessions reconnect), a host running here when it next starts. It helps when `doctor` reports `p2p`
  warn (symmetric NAT) and the router allows UPnP, NAT-PMP or PCP. While there is no mapping it probes the
  gateway every 20 to 26 s (a UPnP search multicast and NAT-PMP/PCP requests), which a managed network may
  flag.
- **Local network only.** `mode = "lan"`, or "Local network only" in the desktop app (Settings >
  Network, which writes it for this computer's host and controller). No relay, no address lookup, no
  network report: the only traffic is between the devices. A host listens on UDP port 47800 (or its
  `port`), and controllers dial each host at the address they stored for it: the host's addresses from the
  pairing ticket, refreshed from every live connection. When a host's address changes (another IP, or a
  host that used a random port before switching to LAN mode), set it on the controller: in the app under
  Settings > Network, or `ai-remote hosts address <host> 192.168.1.20` (no port: 47800). `doctor` on the
  host lists the addresses to use (`lan` check). Pairing with the ticket works as usual; a pair code can
  only be found through local discovery, so with that off the host shows the ticket alone and says why. Devices
  outside this network cannot connect, so set it on both ends only where they share a network (an office,
  a home), and give the host a fixed IP (a DHCP reservation on the router).
- **Fixed port.** Behind a NAT that defeats hole punching (`doctor` reports `p2p` warn), forward a UDP port
  on the router to the host and set `port` to it: controllers then reach the host directly. If the port is
  taken when the host starts, it uses a random port and says so in the log.
- **Quiet mode.** `quiet = true` locks local-network discovery and router port mapping off, whatever
  `lan_discovery`, `port_mapping` or the desktop app's setting say: for a computer on a managed network,
  such as an office, where traffic beyond the connections themselves draws attention. `ai-remote doctor` on
  the host shows what is on (`network` check). Name lookups go only through the system's DNS in every mode:
  one that fails stays failed instead of being asked of Cloudflare, Google or Quad9 (the public resolvers
  are used only when the system has no DNS configured). Paired devices still connect: they find each other
  through the address lookup service, punch a direct path where the networks allow it and are relayed
  otherwise. Where the network's DNS does not resolve the relay or lookup names, nothing connects (list
  self-hosted relays and lookup servers it does resolve). With the lock on, the app shows both switches
  off and disabled, and `ai-remote network discovery on` / `upnp on` refuse with the reason. A pair code
  on screen runs on a short-lived endpoint of its own that follows the same settings (relays, lookup
  servers, port mapping, local network).
- **Address lookup.** Every device publishes its current addresses to n0's lookup service (`dns.iroh.link`)
  every 5 minutes and whenever they change, and looks a peer up there before dialing it. With
  `address_lookup = ["https://dns.example.org/pkarr"]` your own
  [iroh-dns-server](https://github.com/n0-computer/iroh) does both instead. Every device that must find the
  others needs the same servers. Together with self-hosted relays and `relay_fallback_n0 = false`, nothing
  goes to n0.

### What goes over the network

Read from the code (iroh 1.3). Per-process captures on a real office network (2026-10-21, AGENTS.md) found no
ai-remote packet to 5353, 1900 or 5351 with both opt-ins off. The connections themselves are the last row.

| Traffic | To | When | By default |
|---|---|---|---|
| Relay connection | the relays (n0's: `use1-1`, `usw1-1`, `euc1-1`, `aps1-1` `.relay.n0.iroh.link`), HTTPS on TCP 443 | always while running, a ping every 15 s | on |
| Network report | the relays: HTTPS on 443, QUIC address discovery on UDP 7842, a captive-portal check over HTTP on 80 | every 20 to 26 s, a full report every 5 minutes, and on network changes | on |
| Address publishing and lookup | `dns.iroh.link` (or `address_lookup`), HTTPS on 443, and DNS queries through the system | publishing every 5 minutes and on changes; lookup before a dial | on |
| Router port mapping | the gateway: UPnP SSDP multicast to 239.255.255.250:1900, NAT-PMP and PCP on UDP 5351 | every 20 to 26 s while there is no mapping (most office networks have none) | off (Settings > Network) |
| Local-network discovery | mDNS multicast to 224.0.0.251:5353, seen by every device on the network; announcing sends this device's id and addresses (its public one too) | about once a second | off (`lan_discovery`) |
| Public DNS fallback | 1.1.1.1, 8.8.8.8, 9.9.9.9 (UDP 53, HTTPS 443) | when the system has no DNS configured | only then |
| Connections | the other device (UDP, hole punched) or a relay | while paired devices are connected; a keepalive every 5 s; the screen stream is the bulk | on |

Nothing else: no port or address scanning, no update check, no telemetry. The desktop app's interface only
talks to its own process.

## 13. Troubleshooting

**Black or single-colour screenshots.** The `screenshot` result carries a `warning` when the image is
entirely black or one flat colour. Usual causes: the desktop is locked or showing a sign-in or UAC
prompt; the display is off or asleep; an exclusive-fullscreen or DRM-protected app; or the host does
not run in the logged-in user's interactive session. Run `ai-remote doctor` on the host (`session`,
`console`, `desktop`) and check `system_info.session`.

**Locked desktop.** While the desktop is locked or a secure prompt shows, screen capture, focus,
mouse, keyboard, clipboard and mounted tools that need the desktop refuse with the reason, and every
approval is refused. Unlock it. On an unattended test machine turn off automatic locking (screen
saver lock, lock after sleep, dynamic lock), or the host stops being usable whenever it locks.

**Remote Desktop (RDP).** A host inside an RDP session works while the session is connected. When the
RDP client disconnects, the session loses its display (capture and input stop) and the physical
console stays at the lock screen. Before disconnecting, return the session to the console from an
elevated prompt on the host: `tscon <session id> /dest:console` (the `console` doctor check prints the
command with the right id).

**Elevated windows.** Windows silently drops input sent to a program running with higher privileges
(installers, admin tools, UAC-elevated apps). The host refuses such input with a message instead of
pretending it worked. Run the host elevated: install it with `ai-remote host install` from an
administrator's elevated terminal (the task runs with highest privileges). UAC consent prompts
themselves are on the secure desktop and cannot be operated at all.

**Relayed instead of direct.** `list_hosts` (in the MCP client) and `ai-remote hosts --json` show
`path`: `direct`, `relay` or `unknown` for a live connection, plus `relay` (which relay) and `relay_n0` (it is
one of n0's free, rate-limited public relays). `ai-remote hosts` prints the route, and the desktop app (session
toolbar) and the iPhone/iPad app (session bar) say "P2P failed · relayed by n0's public relay" while it lasts.
Relayed traffic works and stays encrypted end to end, but adds latency.
Common causes on the host: a firewall block rule for the host program (doctor `firewall` fails;
`host install` from an elevated terminal removes it), a block rule for all inbound UDP (doctor warns;
review it in `wf.msc`), or networks where UDP hole punching cannot succeed (some corporate and mobile
networks).

**Firewall prompts.** The first time a program listens, Windows asks on the desktop whether to allow
it. On an unattended machine nobody answers, and the unanswered prompt leaves block rules that beat
allow rules, so connections quietly fall back to the relay. `host install` adds the allow rules up
front and removes such stale block rules; `ai-remote host status` lists the rules per program.

**focus_window fails with "foreground lock".** Windows sometimes refuses to bring another app's window
to the front. Click inside the window with `mouse` instead, or ask the user to switch to it.

**The host is not reachable.** On the host: `ai-remote host status` (does it run?) and
`ai-remote doctor` (relay, firewall). On the controller: the error from `list_hosts` or
`ai-remote call system_info` includes the raw cause. Check that both machines are online and that
outbound HTTPS to the relays is allowed.

**The agent does not start.** `ai-remote mcp` and the controller commands report "the controller agent
did not start" with the log path; read `<profile>/controller/controller.log`. Running
`ai-remote agent` in a terminal shows the same messages directly.

**A new host's tools do not show in the MCP client.** The client asked for the tool list before the
host was paired or reachable. Reconnect the client (for example restart the MCP server in its
settings); the tool list is built from the hosts reachable at that moment.

**"a host is already running for this profile".** A host (in a terminal or in the background) already
uses this profile. `ai-remote host status` shows its pid and whether it runs in the background; stop
it with `ai-remote stop-host`, or use another profile with `--home`.
