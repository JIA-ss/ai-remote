# ai-remote 运维指南

中文 | [English](operations.md)

这份指南写给运行被控端（Agent 操作的机器）和控制端（Agent 所在的机器）的人，内容包括安装、配对、访问控制，
用来了解现状的体检和日志，以及调用被拒绝或出问题时该怎么办。

> 状态：Pre-alpha 阶段。M0 的目标是 Windows 被控端；macOS 被控端规划在 M1。
> 正式发布前命令仍可能调整。

相关文档：设计见 [architecture.zh-CN.md](architecture.zh-CN.md)；每个工具和各类配置格式见
[tools.zh-CN.md](tools.zh-CN.md)。

## 目录

1. [命令一览](#1-命令一览)
2. [配置目录与文件](#2-配置目录与文件)
3. [运行被控端](#3-运行被控端)
4. [控制端常驻进程](#4-控制端常驻进程)
5. [配对步骤](#5-配对步骤)
6. [权限级别与访问控制](#6-权限级别与访问控制)
7. [被控端拒绝了怎么办](#7-被控端拒绝了怎么办)
8. [体检（doctor）](#8-体检doctor)
9. [审批](#9-审批)
10. [审计日志](#10-审计日志)
11. [被控端配置文件](#11-被控端配置文件)
12. [跨网络连接](#12-跨网络连接)
13. [故障排查](#13-故障排查)

## 1. 命令一览

所有命令都接受 `--home <DIR>`（换一个配置目录，见第 2 节）和 `--json`（输出机器可读的 JSON；`host`、
`host install`、`host uninstall`、`mcp`、`agent`、`stop-agent` 会忽略它）。这两个选项放在子命令前后都可以。
其余用法见 `ai-remote <命令> --help`。

**被控端上：**

| 命令 | 作用 |
|---|---|
| `ai-remote host [--no-pair-code]` | 在当前终端运行被控端，直到按 Ctrl+C 或执行 `stop-host` |
| `ai-remote host install` | 登录时自动在后台启动被控端（`--service`：作为能到达锁屏的 Windows 服务） |
| `ai-remote host uninstall` | 删除自启项、停止被控端、删除防火墙规则 |
| `ai-remote host status` | 查看自启项、防火墙规则，以及被控端是否在运行 |
| `ai-remote host start` | 立即启动已安装的后台被控端（比如 `stop-host` 之后）；桌面端的托盘也用它。Windows 服务要在管理员终端里运行 |
| `ai-remote doctor` | 分级体检：这台机器到底能不能被操作？ |
| `ai-remote pair-code [--level L] [--name N]` | 显示一次性配对码（二维码和 8 位数字）以及它背后的票据 |
| `ai-remote peers` | 已配对的控制端，以及各自的级别和设备 ID；被撤销的标为 `REVOKED` |
| `ai-remote level <设备> <observe\|control\|admin\|none>` | 修改某个控制端的级别 |
| `ai-remote revoke <设备>` / `unrevoke <设备>` | 拒绝某个控制端，或解除拒绝 |
| `ai-remote forget <设备>` | 删除某个控制端的配对 |
| `ai-remote stop-host` | 停止正在运行的被控端 |
| `ai-remote bypass [on\|off <设备>\|--all] [--for 2h]` | 完全信任控制端：全部工具、无需审批、不受 deny 规则限制（6.1 节） |
| `ai-remote bypass default on\|off` | 默认信任所有已配对的控制端（含以后配对的），直到你单独关掉某一台（6.1 节） |
| `ai-remote approvers [add\|remove]` | 审批密钥：哪些人可以在已配对设备上审批（见 [approvals.zh-CN.md](approvals.zh-CN.md)） |
| `ai-remote mount list\|add\|add-profile\|remove\|test` | 管理挂载的 MCP server（见 [tools.zh-CN.md](tools.zh-CN.md#7-挂载-mcp-server)） |

**控制端上：**

| 命令 | 作用 |
|---|---|
| `ai-remote pair <配对码\|票据> [--name N]` | 用配对码（`4821 0937`）或票据与被控端配对 |
| `ai-remote hosts` | 已配对的被控端，以及配对时各自授予的级别；默认被控端标为 `(default)` |
| `ai-remote hosts default [<被控端>\|--clear]` | 查看、设置或清除默认被控端：命令和 Agent 没指定被控端时用它 |
| `ai-remote network [mode auto\|lan \| discovery on\|off \| upnp on\|off]` | 查看或修改这台电脑的网络设置，被控端和控制端一起改（即桌面端 设置 > 网络；第 12 节） |
| `ai-remote hosts address <被控端> [<地址>]` | 查看或设置去哪个地址连某个被控端（`ip`、`ip:端口`，多个用逗号分隔）；仅限局域网模式下被控端地址变了，靠它重新连上（第 12 节） |
| `ai-remote unpair <被控端> [--local]` | 解除配对：能连上时被控端也会删掉本设备，然后本控制端忘掉它（`--local`：只在本机忘掉） |
| `ai-remote access [--host H]` | 本设备在某台被控端上能做什么：你的级别、与你有关的长期规则、已配对的设备（管理级别能看到全部）和可用的工具 |
| `ai-remote lower <级别> [--host H]` | 降低本设备在某台被控端上的级别；要再提升，需要那台电脑前的人执行 `ai-remote level` |
| `ai-remote call <工具> ['{JSON 参数}'] [--host H]` | 调用一个工具并打印结果 |
| `ai-remote exec`、`job`、`shot`、`windows`、`info`、`doctor --host`、`ls`、`stat`、`mkdir`、`rm`、`push`、`pull` | 在终端或脚本里操作被控端（4.1 节） |
| `ai-remote watch [--host H]` | 实时跟踪被控端的活动：先是最近的记录，然后是发生中的每次调用和会话 |
| `ai-remote view [--host H] [--seconds N] [--out F]` | 观看被控端屏幕一段时间并报告视频流情况（帧数、尺寸、帧率）；`--out` 保存 H.264 |
| `ai-remote approvals` / `approve <id>` / `deny <id>` | 列出、允许或拒绝等待审批的调用（见 [approvals.zh-CN.md](approvals.zh-CN.md)） |
| `ai-remote approver [--create] [--enroll <被控端>]` | 本设备的审批密钥（`--create` 生成），或把它提交给被控端 |
| `ai-remote take-over [--let-watch]` / `hand-back [--note N]` | 从 Agent 手里接管，以及交还控制（9.3 节） |
| `ai-remote pause` / `resume` | 暂停某台被控端上的 Agent，直到你恢复 |
| `ai-remote tell <文字>` | 给 Agent 留言；它会随下一次调用结果送达 |
| `ai-remote mcp` | 给 MCP 客户端用的 stdio MCP server |
| `ai-remote agents [--disconnect ID]` | 当前连接到本控制端的智能体（MCP 客户端）及其调用次数；`--disconnect` 断开其中一个（应用“设置 → 智能体”做的是同一件事） |
| `ai-remote agent` / `stop-agent` | 在前台运行控制端常驻进程，或停止它 |

**两端通用：** `ai-remote status` 显示当前配置目录，以及它对应的被控端和常驻进程是否在运行（附设备 ID）。

## 2. 配置目录与文件

所有状态都放在每个用户自己的配置目录里：

| 平台 | 默认位置 |
|---|---|
| Windows | `%LOCALAPPDATA%\ai-remote\data`（本地目录，不随漫游配置同步，以免同一个设备身份被复制到多台机器上） |
| macOS | `~/Library/Application Support/ai-remote` |

`--home <DIR>` 或环境变量 `AI_REMOTE_HOME` 可以指定别的目录。一台机器上可以同时跑多个配置目录，
各有自己的身份、被控端和常驻进程；测试和本地开发就是这样在同一台电脑上同时跑被控端和控制端的。
给 MCP 客户端用时，把这个选项加到它启动的命令行里：`ai-remote --home D:\profiles\lab mcp`。

```
<配置目录>/
  host/
    identity.key         被控端的设备私钥，像密码一样保管
    mesh/                传输层状态：配对、级别、撤销记录、尚未兑换的票据
    ai-remote.toml       被控端配置（第 11 节）
    audit.jsonl          审计日志（第 10 节）
    host.log             后台被控端的日志，轮转为 host.log.1、host.log.2
    autostart-task.xml   `host install` 写出的任务定义（Windows）
    host-stderr.log      LaunchAgent 的标准错误输出（macOS）
    jobs/                每个后台任务的 <id>.log 和 <id>.json
    mounts/              挂载 server 的工作目录：<name>.log、<name>.tools.json
  controller/
    identity.key         控制端的设备私钥
    mesh/                传输层状态：配对
    hosts.json           每台被控端在配对时授予的级别和配对时间
    controller.log       按需启动的常驻进程的日志
```

每个运行中的后台进程都在一个只有同一用户能打开的本地控制通道上监听：Windows 上是命名管道
`\\.\pipe\ai-remote-host-<id>` 和 `\\.\pipe\ai-remote-controller-<id>`，其他系统上是 `host/control.sock`
和 `controller/control.sock`。`<id>` 是配置目录路径的哈希（16 位十六进制），所以不同配置目录不会冲突。
命令行就是通过它们找到正在运行的被控端或常驻进程的。

日志：`AI_REMOTE_LOG` 接受日志过滤表达式（例如 `AI_REMOTE_LOG=debug` 或 `info,ar_host=debug`）。
命令把日志写到 stderr（`ai-remote mcp` 的 stdout 留给协议）；后台被控端和按需启动的常驻进程写到各自的日志文件。

## 3. 运行被控端

被控端必须运行在已登录用户的交互式桌面会话里，从不做成系统服务：Windows 的服务运行在 0 号会话，
那里没有桌面，截图会是黑的，输入没有去处，审批框也弹不出来。

### 3.1 在终端里运行

```sh
ai-remote host
```

它会打印自己的名称、设备 ID、配置目录、已配对的控制端数量和审计日志路径。还没有配对任何控制端时，
它还会显示一个配对码，连同二维码和票据（级别 `control`）；加 `--no-pair-code` 可以不显示。如果眼下弹不出审批框（桌面已锁定），
它会警告：需要审批的操作都会被拒绝。

用 Ctrl+C 停止，或在另一个终端执行 `ai-remote stop-host`。停止时，所有控制端连接都会以 `host stopping` 为原因关闭。
每个配置目录只能运行一个被控端：再执行一次 `ai-remote host` 会报错 `a host is already running for this profile`。

### 3.2 后台运行：安装与登录自启

**在桌面端里**（不用终端）：设置 → 本设备 →“开启远程控制”。应用先列出会做的改动，然后由系统要求管理员授权（macOS 输密码，Windows
UAC），包内的 `ai-remote` 安装随应用附带的被控端。“关闭”以同样方式移除；macOS 上缺哪项权限，就有对应的“授权”按钮弹出系统自己的请求。
下面的命令是在终端里做同样的事。

```sh
ai-remote host install --dry-run   # 列出每一处改动，但什么都不改
ai-remote host install
```

重复执行 `host install` 是安全的；移动或更新程序文件后，再执行一次即可完成升级。`ai-remote-hostw`
必须和 `ai-remote` 放在同一个目录：自启项运行的就是它。

#### Windows

谁来执行安装：

| 你的账户 | 在哪里执行 | 结果 |
|---|---|---|
| 管理员 | 以管理员身份运行的终端 | 登录任务和防火墙规则 |
| 管理员 | 普通终端 | 拒绝执行，并给出以管理员身份运行的命令（任务要以“最高权限”运行，需要提升；防火墙规则本来也需要） |
| 标准用户 | 普通终端 | 只创建登录任务；会打印一条命令，交给管理员添加防火墙规则。被控端此后以该用户自己的权限运行，无法操作以管理员身份运行的窗口（doctor 会警告） |
| 管理员 | 以管理员身份执行 `host install --firewall-only` | 只添加防火墙规则，针对本次安装的程序（对所有账户生效） |

如果以管理员身份运行的终端用的不是正在使用桌面的那个账户（例如标准用户的终端用管理员的凭据提升），
安装会被拒绝：否则任务会在管理员登录时、用管理员的配置目录启动被控端。正确的做法是：在用户自己的普通终端里执行
`ai-remote host install`，再请管理员执行 `host install --firewall-only`。加 `--force` 可以照样为提升后的账户安装；
被控端会在该账户下次登录时启动，现在不会启动。

安装做了什么（`--dry-run` 会全部打印出来，包括完整的任务 XML）：

1. 把任务定义写到 `<配置目录>/host/autostart-task.xml`，再用 `schtasks.exe`（按绝对路径调用）注册为
   `\ai-remote\host-<8 位十六进制>`，每个配置目录一个任务。这个任务：
   - 在你的账户登录时启动（按 SID 识别，改用户名不影响）；
   - 以交互式令牌在你的桌面会话里运行，使用最高权限（这样被控端才能操作以管理员身份运行的窗口）；
   - 运行 `ai-remote-hostw.exe --home "<配置目录>"`；
   - 不限运行时长（默认设置会在 3 天后停止任务），使用电池时既不停止也不阻止启动，错过了登录时的启动会在之后补上，
     已有实例在运行时忽略新的启动请求。
2. 防火墙（需要管理员）：删掉这两个程序当前路径下以前的 `ai-remote host` 规则（程序已移出的旧目录对应的规则会留下，
   请在 `wf.msc` 里手动删除）；删掉针对这两个程序的过期阻止规则（通常是某个没人回答的“是否允许访问”提示留下的）；
   然后为 `ai-remote-hostw.exe` 和 `ai-remote.exe` 添加名为 `ai-remote host` 的入站允许规则，覆盖 UDP 和 TCP、
   所有网络类型。针对所有程序入站 UDP 的阻止规则只报告、不改动（请在 `wf.msc` 里检查）。
3. 除非加了 `--no-start`，否则启动任务，最多等 20 秒让被控端响应，然后打印它的 pid、日志路径，以及被控端自己的
   体检报告作为自检；如果还没有任何配对，再提示下一步（配对）。如果这个配置目录已经有被控端在运行（在终端里，
   或之前启动的后台被控端），就不去动它，新的自启项从下次登录开始接管；想现在就切换，先执行 `ai-remote stop-host`，
   再执行一次 `ai-remote host install`。

某一步失败时，后面的步骤都不会执行；解决原因后重新执行同一条命令即可。

#### Windows：作为服务运行（锁屏、登录界面、UAC 提示）

```sh
ai-remote host install --service --dry-run   # 管理员终端；列出每一处改动，但什么都不改
ai-remote host install --service
```

上面的登录任务以你的身份、在你的桌面会话里运行被控端。Windows 不让*用户*进程接触安全桌面：一旦锁屏、弹出 UAC
提示、或者重启后停在登录界面，这个被控端就既看不到也操作不了任何东西，只能等有人在键盘前解锁。`--service` 改为注册
一个 Windows 服务（RustDesk 走的是同一条路）：服务以 LocalSystem 身份运行、自动启动，并让 `ai-remote-hostw`
**以 SYSTEM 身份在控制台会话里**一直运行。该会话里的 SYSTEM 进程可以打开安全桌面，所以实时画面（服务模式下自动
选用 Desktop Duplication 采集；看同一块屏的观看者共用一个复制，最多 3 个观看者可以同时看）、截图和输入都能跟上
锁屏、UAC 提示和重启后的登录界面；控制端可以在锁屏上输密码，也可以按 `ctrl+alt+del`（被控端请服务代发，因为
直接按键会被 Windows 忽略）。服务会跟着控制台会话走（注销、切换用户），崩溃后按守护进程同样的退避重启被控端，
`ai-remote stop-host` 正常退出后服务也一起停。

**安装前必读的后果：** 这个被控端，连同它的 `shell`、`jobs`、`files` 工具，都以 SYSTEM 身份运行。级别、策略、
审批、审计日志的代码都没变，但一个 admin 级（或 bypass）控制端能做的事，从"你能做的"放大到"SYSTEM 能做的"。
授予级别时要考虑到这一点。`--dry-run` 会把这段话和计划一起打印出来。锁屏时审批弹窗仍然显示不了（没人在那里回答）：
锁屏期间需要审批的操作只能由审批设备批准（第 9.2 节）。

安装做了什么（全部是管理员步骤；标准用户会被拒绝）：

1. 如果这个配置目录有登录任务，先结束并删除它（两者都会启动被控端，后启动的会发现配置目录已被占用）。
2. 用 `sc.exe`（绝对路径）注册或更新服务 `ai-remote-host-<8 位十六进制>`：LocalSystem、自动启动、运行
   `ai-remote-hostw.exe --service --home "<配置目录>" --owner-sid <你的 SID>`，崩溃后由服务管理器重启。
   `--owner-sid` 是你的账户：被控端的控制端点（只允许属主的命名管道）除 SYSTEM 外也允许它，所以
   `ai-remote host status`、`stop-host`、`doctor` 和桌面端对你照常可用；其他账户和以前一样被拒。
3. 当 `SoftwareSASGeneration` 策略（`HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System`）
   不是 1 或 3 时把它设为 1（"服务"），这样服务才能发送 Ctrl+Alt+Del。原值写到
   `<配置目录>/host/sas-policy-before.txt`，`host uninstall` 会改回去。
4. 和登录任务安装相同的防火墙步骤。
5. 除非加了 `--no-start`，否则启动服务并像上面一样等待被控端响应。

之后 `ai-remote host status` 显示 `Windows service ai-remote-host-… is installed, running`。
`ai-remote host uninstall` 停止并删除服务、恢复策略，再做和以前一样的任务与防火墙步骤。`stop-host` 会把被控端
*和*服务一起停掉（正常退出结束两者，和守护进程一致）；再执行一次 `ai-remote host install --service` 或
`sc start ai-remote-host-…` 即可恢复。服务运行期间在终端里启动的被控端会占住配置目录，服务每 30 秒重试一次，
等那个被控端停了再接管。

锁屏的机器从控制端看是什么样：`doctor` 的 `desktop` 项为 **warn**（"locked or showing a security prompt …;
this host can see and operate it"），`approval` 为 warn，工具照常可用，画面是锁屏界面。没有 `--service` 时同样的
状态是 **fail**，所有截图和输入工具都会以 "which this host cannot reach" 拒绝。

#### macOS

安装会写一个用户级 LaunchAgent
`~/Library/LaunchAgents/io.github.jia-ss.ai-remote.host.<8 位十六进制>.plist`，并用
`launchctl bootstrap gui/<uid>` 加载。它在登录时运行 `ai-remote-hostw --home <配置目录>`，只在图形（Aqua）
会话中、以交互进程运行；崩溃后 launchd 会重启它，正常退出则不会，所以执行 `ai-remote stop-host` 之后，
它会一直保持停止，直到下次登录。安装不需要管理员权限。`ai-remote-hostw` 第一次截屏或操作时，macOS
会请求“屏幕录制”和“辅助功能”权限，在“系统设置 > 隐私与安全性”里都允许即可。

**锁屏**对这个被控端是可达的：屏幕锁定时，`doctor` 的 `desktop` 项是 **warn**（“the desktop is locked …
(input desktop: lock screen); this host can see and operate it”），控制端能看到锁屏画面、能输入密码；需要审批的
操作会被拒绝（没人在场回答；审批设备仍然可以批准）。macOS 上做到这一步不需要 `--service`。

**登录界面**（重启或注销之后）是另一个会话，用户级 agent 进不去。`host install --service` 改写一个*系统级*
LaunchAgent：`/Library/LaunchAgents/io.github.jia-ss.ai-remote.host.<8 位十六进制>.plist`，
`LimitLoadToSessionType` 为 `LoginWindow` 加 `Aqua`（RustDesk 就是这样到达登录界面的），并移除用户级的那份。
这个文件归 root，所以要用 `sudo` 运行：

```sh
sudo ai-remote --home "$HOME/Library/Application Support/ai-remote" host install --service --dry-run
sudo ai-remote --home "$HOME/Library/Application Support/ai-remote" host install --service
```

`--home` 指定你的配置目录（sudo 会改掉 `HOME`）；计划从配置目录的属主读出账号。在登录界面的会话里 launchd
以 root 启动被控端，被控端随即切换成安装它的用户（plist 里写了 `--owner-uid`），这样配置目录仍归该用户，
该用户给 `ai-remote-hostw` 的“屏幕录制”“辅助功能”授权也仍然有效。在用户自己的会话里，同一个 agent 就以
该用户运行，和用户级 agent 完全一样。*尚未在真机验证*：从 root 降到用户的进程在登录界面能否拿到
ScreenCaptureKit 的画面。如果 `doctor` 说桌面可用但那里的画面一直是黑的，在 agent 的环境里设
`AI_REMOTE_KEEP_ROOT=1`（plist 加一项 `EnvironmentVariables`），让它像 RustDesk 一样在登录界面以 root 运行，
并反馈。`sudo ai-remote … host uninstall` 移除系统级 agent；不加 sudo 的卸载会在这一步停下并说明。

**快速用户切换与注销。** launchd 为每个图形会话各起一份系统级 agent：你的会话里一份，登录界面一份。
快速用户切换（用户菜单里的“登录窗口…”）时，你的会话在后台继续运行、屏幕显示登录界面，所以两份会同时运行、
用同一个配置目录。由一条规则决定谁来服务：**只有自己会话在控制台上的那一份持有配置目录。** 持有的那份每秒
检查一次，连续两次读到自己会话已离开控制台，就关闭控制端连接，在仍持有配置目录的情况下以退出码 4 退出，
控制端点和传输层在同一时刻释放。另一份一直在轮询，一秒内拿到配置目录并启动传输层。控制端看到的是一次断开，
随后重连到同一台设备。切回原会话时反过来同样处理。真正的注销会结束你的会话和其中那一份，登录界面那一份随后
接手。锁屏仍是你的会话、仍在控制台上，什么都不变。预计交接耗时：约 2-3 秒察觉切换，加上传输层启动时间
（推导值，*尚未在真机实测*）。

等待中的那一份不退出、也不会每 10 秒被重启一次；每次状态变化只写一行日志，例如：

```text
waiting: this session is not on the console (the screen shows another session or the login window)
waiting: another host holds this profile (the other session's instance, or a host in a terminal)
this session has the console: taking over the profile
stepping aside: this session left the console; the instance that has it takes over
not starting: the host was stopped with `ai-remote stop-host` (see `host status`)
```

需要知道的后果：

- 终端里运行的被控端（`ai-remote host`）占着配置目录时，agent 只写一行 `waiting` 并等待；终端被控端退出后
  （Ctrl+C 或 `stop-host` 都一样，只有系统级 agent 会记录停止），agent 接手。
- 对系统级 agent 执行 `ai-remote stop-host` 仍然是停止：停止的被控端会写下 `<配置目录>/host/stopped`，另一份
  不会接手。在你的会话里做的停止，在同一个登录会话里（锁屏、切回原会话）以及登录界面上一直有效，直到下一次
  真正登录或重启；在登录界面被服务期间做的停止，在所有会话里一直有效，直到重启。`host install`（以及
  `host uninstall`）会清除它，`host status` 会说明它是否生效。如果写不了这个记录，`stop-host` 会被拒绝，
  而不是悄悄交给另一份。授予隐私权限后要重启 agent，请用 `launchctl kickstart -k gui/<uid>/<label>`，
  不要用 `stop-host`。
- 登录界面那一份降权后读到的会话状态尚未在真机验证。每一份在读数变化时都会记一行 `console read`。读不到会话
  状态时，登录界面那一份改看 `/dev/console` 的属主（登录界面占屏时是 root），一旦某个用户会话占了屏幕就让出
  配置目录，即使 launchd 没有结束它。
- 传输层数据库连续 15 秒打不开（正常情况下是上一个持有者还在退出）时，这一份以退出码 1 停止，由 launchd
  按节流重新拉起。
- 不在控制台上的会话（比如通过“屏幕共享”打开的会话）永远不会服务这个配置目录：那里的被控端会让出。
- 其他用户的会话也会起一份（以那个用户身份），它会立即退出，不碰所有者的配置目录。
- `AI_REMOTE_KEEP_ROOT=1` 与这套交接一起使用未经测试：登录界面那一份会以 root 创建控制锁文件和停止记录，你的
  会话里那一份可能打不开它们。

#### 查看与卸载

```sh
ai-remote host status
```

会打印自启项及其运行的内容（以及是不是本次安装、本配置目录的），每个被控端程序的入站防火墙规则
（允许了哪些协议、有哪些阻止规则生效），防火墙是否关闭，以及被控端是否在运行（pid，在后台还是在终端）。

```sh
ai-remote host uninstall --dry-run
ai-remote host uninstall
```

会停止正在运行的被控端（最多等 10 秒），结束并删除任务（macOS 上是卸载并删除 LaunchAgent），删除任务 XML，
并删除防火墙规则。有两种情况防火墙规则会保留：另一个配置目录的任务还在启动同一个 `ai-remote-hostw`
（规则是按程序建的，要留给它用）；或者无法列出其他任务（这时输出里会给出删除规则用的 `netsh` 命令）。
权限要求和安装相同：标准用户卸载时会留下防火墙规则，由管理员执行 `ai-remote host uninstall --firewall-only` 删除。

### 3.3 后台被控端：守护进程、日志与重启

`ai-remote-hostw` 就是不带控制台窗口的被控端。终端版会打印的内容都写到 `<配置目录>/host/host.log`，
而且它从不自行生成配对票据。

- **守护进程（Windows）。** 任务启动的其实是一个小守护进程，它把被控端作为子进程运行在一个 kill-on-close
  的作业对象（job object）里。被控端异常退出（崩溃、panic、出错）时，守护进程等 5 秒后重新启动它，
  每次等待时间翻倍，最长 5 分钟；连续运行满 10 分钟就算健康，等待时间重新从 5 秒算起。正常停止
  （`ai-remote stop-host`）会让两者一起退出。结束任务或守护进程意外退出，都会结束被控端以及作业对象里的所有进程。
  守护进程的作业对象允许后台任务脱离，但任务计划程序给每个任务套的作业对象不允许，所以后台被控端进程只要结束
  （停止、崩溃、重启），它启动的后台任务都会一起结束。
  （任务计划程序自带的“失败时重新启动”只管任务启动失败，不管程序带着错误退出。）macOS 上由 launchd 的
  `KeepAlive` 做同样的事。
- **每个配置目录一个被控端。** 再启动一个（双击、脚本）会发现配置目录已被占用，在日志里写下
  `not starting: a host is already running for this profile`，并以退出码 3 退出，不会影响正在运行的被控端。
- **日志。** `host.log` 超过 8 MiB 时轮转，保留前两份（`host.log.1`、`host.log.2`）。日志文件打开之前发生的错误
  写到临时目录下的 `ai-remote-hostw-error.log`。macOS 上 launchd 还会把进程的 stderr 写到 `host/host-stderr.log`。
- **`stop-host` 之后重新启动：** `ai-remote host start`（或桌面端托盘）；注销再登录；或者再执行一次 `ai-remote host install`（没有被控端在运行时它会启动任务）；
  或者自己启动任务：`schtasks /Run /TN "ai-remote\host-<8 位十六进制>"`（任务名见 `ai-remote host status`）。
- **不弹控制台窗口。** 后台被控端没有控制台，所以它启动的所有东西（shell 命令、后台任务、挂载的 server、系统工具）
  都不带控制台窗口，用户桌面上不会突然弹出窗口。

## 4. 控制端常驻进程

控制端由两个程序组成：

- `ai-remote agent` 是每个用户一个的常驻进程。它持有控制端身份和配对信息，对每台被控端保持一条连接；
  每个 MCP 会话都是这条连接上的一条流。
- `ai-remote mcp` 是 MCP 客户端启动的程序。它连到常驻进程（需要时先启动它），把 stdio 转发过去，
  客户端关闭 stdin 后退出。

常驻进程按需启动：控制端命令（`pair`、`hosts`、`call`、`exec`、`mcp` 等）发现它没在运行时，都会在后台把它拉起来
（最多等 20 秒），输出追加到 `<配置目录>/controller/controller.log`。连续 30 分钟没有任何客户端时它会自动退出；
打开着的 MCP 会话也算客户端。`ai-remote stop-agent` 立即停止它；`ai-remote agent` 在前台运行它，方便调试
（这个配置目录已有常驻进程在运行时会报错）。

把 ai-remote 加到 MCP 客户端里：

```sh
claude mcp add ai-remote -- ai-remote mcp
```

其他能启动 stdio 服务的 MCP 客户端也一样：命令写 `ai-remote`，参数写 `["mcp"]`（换配置目录就再加
`"--home", "<DIR>"`）。可以同时运行多个客户端，它们共用同一个常驻进程和它的连接。

脚本里可以用 `ai-remote call <工具> '<JSON 参数>' [--host <名称>]` 调用一个工具并打印结果（图片只打印一行摘要）。
工具报错（包括被拒绝）时退出码为 2；调用根本没能发出（被控端连不上、参数有误）时退出码为 1。加 `--json`
则打印原始结果，不使用退出码 2。每次 `call` 都是一个新的 MCP 会话，所以上一次 `call` 拿到的截图 ID，下一次用不了。

### 4.1 用命令行操作被控端

`ai-remote call` 能调用任何工具，但 JSON 参数和原始结果不适合日常使用。下面这些命令覆盖人和脚本最常做的事，
参数和输出都是普通的命令行形式。每条命令都是经常驻进程发出的一次或几次工具调用，所以被控端的级别、策略、
审批和审计对它们的作用和对 Agent 的调用完全一样。加 `--json` 输出机器可读的 JSON。

| 命令 | 对应工具 | 级别 |
|---|---|---|
| `ai-remote info` | `system_info` | observe |
| `ai-remote doctor --host H` | `doctor` | observe |
| `ai-remote shot [-o F.png] [--monitor N] [--max-long-edge PX]` | `screenshot` | observe |
| `ai-remote windows [--filter T] [--all]` | `list_windows` | observe |
| `ai-remote ps [<名字>] [--pid N] [--limit N]` | `process_list` | admin |
| `ai-remote kill --pid N \| --name X [--no-tree] [--intent 原因]` | `process_kill` | admin，默认要审批 |
| `ai-remote exec "<命令>" [--shell S] [--cwd D] [--timeout S] [--env K=V] [--stdin F]` | `shell` | admin，默认要审批 |
| `ai-remote job start "<命令>"` / `list` / `status <id>` / `log <id> [-f]` / `wait <id> [--timeout S]` / `kill <id>` | `jobs_*` | admin；`start` 要审批 |
| `ai-remote ls <目录> [--pattern P]` / `stat <路径>` | `files_list` / `files_stat` | admin |
| `ai-remote mkdir <目录>` / `rm <路径> [-r]` | `files_mkdir` / `files_delete` | admin，默认要审批 |
| `ai-remote push <本地> <远端> [--force] [--mkdir]` | `files_push` + 文件通道 | admin，默认要审批 |
| `ai-remote pull <远端> [<本地>] [--force]` | `files_pull` + 文件通道 | admin，默认要审批 |
| `ai-remote chat list [--refresh]` | `chat_runtimes` | admin |
| `ai-remote chat start <智能体> --cwd <目录> [--prompt ..] [--model ..] [--effort ..] [--hardened]` | `chat_start` + 对话通道 | admin，默认要审批 |
| `ai-remote chat run <智能体> "<任务>" --cwd <目录> [--keep]` | `chat_start` + 对话通道，打印回答 | admin，默认要审批 |
| `ai-remote chat sessions` / `attach <id>` / `send <id> "<文本>" [--wait]` / `interrupt <id>` / `end <id>` | `chat_sessions`、`chat_attach`、`chat_interrupt`、`chat_end` | admin |
| `ai-remote chat send <id> "<文本>" --image <文件>` / `commands <id>` / `files <id> [查询]` | 对话通道：随消息发图片、列出智能体的斜杠命令和技能、列出会话目录下的文件 | admin |
| `ai-remote chat fork <id> [--prompt ..]` | 带 `fork_of` 的 `chat_start`：新会话带着对话的副本（Claude Code、Codex），打印新会话 | admin，询问 |
| `ai-remote chat search "<词>" [--limit N]` | `chat_search`：消息里包含每个词的会话，带摘录 | admin |

**发往哪台被控端。** 依次取 `--host <名称>`、环境变量 `AI_REMOTE_HOST`、本设备的默认被控端、唯一配对的被控端。
`ai-remote hosts default <名称>` 设置默认被控端（保存在 `controller/hosts.json`；MCP 会话里调用没指定 `host`
且没用过 `use_host` 时也用它），`ai-remote hosts default --clear` 清除，`ai-remote hosts` 会把它标出来。
`AI_REMOTE_HOST` 对所有带 `--host` 的控制端命令生效，但不影响 `doctor`：不带 `--host` 时它检查的是本机。

**退出码。** 0：完成。2：被控端拒绝了调用或工具执行失败（被控端的说明打印在 stderr）。1：调用没能发出
（常驻进程起不来、被控端连不上、参数有误）。`exec` 和 ssh 一样直接用远端命令自己的退出码：超时被结束为 124，
根本没有执行为 255。`job wait` 用任务的退出码（到 `--timeout` 时仍在运行为 124；被结束为 137；退出码未知为 255，
例如被控端重启后重新接管的任务）。

**文件。** 文件命令用被控端上的绝对路径。`push` 先在本地算出文件的 SHA-256，请被控端批准恰好这个大小和哈希，
再流式发送；被控端写到目标旁边的临时文件，大小和哈希都对上才原子替换到目标位置，传输中断不会留下写了一半的
目标文件。远端路径以 `/` 或 `\` 结尾、或者指向一个已存在的文件夹时，文件按原名放进去；`--mkdir` 创建缺少的
文件夹，`--force` 替换已有文件。`pull` 流式写到本地临时文件，和被控端给出的 SHA-256 一致才保留；本地文件已存在
且没加 `--force` 时，在询问被控端之前就拒绝。每次传一个文件，文件夹请先打包（`tar -czf`）。每次传输都写进审计日志
（`transfer` 记录：路径、字节数、耗时、结果）。

**审批。** `exec`、`job start`、`mkdir`、`rm`、`push`、`pull` 默认都要机主审批，和 Agent 的调用一样。
自己用控制端操作的测试机，可以在那台被控端上加一条只针对本设备的规则，省掉审批框：

```toml
[[rule]]
tool = "files_*"
principal = "my-laptop"   # 本控制端在被控端 `ai-remote peers` 里的名字
action = "allow"
```

示例：

```sh
ai-remote hosts default lab-pc
ai-remote info
ai-remote exec "Get-Process game -ErrorAction SilentlyContinue | Select-Object Id, WS"
ai-remote push build.zip 'D:\deploy\' --mkdir
ai-remote exec "Expand-Archive D:\deploy\build.zip D:\deploy\build -Force" --timeout 300
ai-remote job start "D:\deploy\build\game.exe" --intent "性能基线"
ai-remote job log <id> -f
ai-remote shot -o lab.png
ai-remote pull 'D:\captures\frame.ngfx-gputrace' traces/
```

## 5. 配对步骤

1. **确认被控端在运行**（在被控端执行 `ai-remote host status` 或 `ai-remote status`）。
2. **在被控端显示配对码**（桌面应用“本设备 → 显示配对码”，或托盘里的“显示配对码”），或在终端执行：

   ```sh
   ai-remote pair-code --level control --name laptop
   ```

   `--level` 可以是 `observe`、`control`（默认）或 `admin`：先给够用的最低级别。`--name` 是被控端给来配对的
   控制端起的名字（默认用控制端建议的名字，也就是它的机器名）。它会显示一个二维码、一个 8 位配对码和背后的票据。
   配对码只能用一次，10 分钟内有效，且只在被控端运行期间有效；显示新码时旧码作废；5 个没能证明配对码的连接会让它作废。
3. **把配对码交给另一台设备**：当面给，或走可信的渠道。有效期内它就是凭证：谁先用，谁就拿到这个级别。
4. **在控制端配对**：用 iPhone 应用扫二维码，或在桌面应用的“配对电脑”页输入数字，或执行：

   ```sh
   ai-remote pair 4821 0937 --name office-pc
   ```

   `--name` 是你在本地给这台被控端起的名字（默认用被控端建议的名字）。命令会打印被控端的名称、
   由三个英文单词组成的安全码，以及授予的级别。控制端通过连接本身用的发现机制（n0 的 DNS/pkarr 服务、局域网 mDNS、中继）
   找到配对码背后的被控端；如果两台机器这样也互相够不着（没有互联网、也不在同一网络），就改用票据：
   `ai-remote pair <票据>`，只能用一次，24 小时内有效。
5. **核对安全码**，和被控端显示的比一比：
   - 在终端里运行的被控端会打印 `Paired with controller "laptop" (level: control).` 和 `Safety code: ...`；
   - 后台被控端会在日志里写一行 `controller paired`，带有 `peer=`、`level=` 和 `sas=`。在 Windows 上可以这样查：

     ```powershell
     Select-String -Path "$env:LOCALAPPDATA\ai-remote\data\host\host.log" -Pattern 'controller paired' |
       Select-Object -Last 1
     ```

6. **如果单词不一致**，说明票据被别人兑换了。在被控端执行 `ai-remote forget <名称>`（或者用 `revoke`
   永久拒绝那台设备），在控制端执行 `ai-remote unpair <被控端>`，然后用新票据重新来过。
7. **检查结果：** 在控制端执行 `ai-remote hosts`，在被控端执行 `ai-remote peers`，再执行 `ai-remote call system_info`
   （配对了多台被控端时加 `--host office-pc`）。

名称规则：转为小写，ASCII 字母和数字以外的字符都变成短横线，连续的短横线合并为一个，去掉首尾的短横线，
最后截断到 40 个字符（`Office PC (2)` 变成 `office-pc-2`）。凡是以控制端或被控端为参数的命令，都可以用名称或
`eid:` 设备 ID；`level`、`revoke`、`unrevoke`、`forget`、`unpair` 和 MCP 的 `host` 参数还接受两者中任一个的唯一前缀。

## 6. 权限级别与访问控制

| 级别 | 控制端能用的工具 |
|---|---|
| `observe` | `system_info`、`doctor`、`screenshot`、`zoom`、`list_windows`，以及挂载 server 中 observe 级的工具 |
| `control` | 再加 `focus_window`、`mouse`、`keyboard`、`clipboard_read`、`clipboard_write`，以及挂载 server 中 control 级的工具 |
| `admin` | 再加 `shell`、`jobs_*`、`files_*`，以及挂载 server 中 admin 级的工具 |

控制端只能看到它的级别允许的工具。下面几条命令都在被控端执行，立即生效；而 `ai-remote.toml` 的改动
（策略规则、挂载、审批超时、中继、名称）要重启被控端才生效。

| 命令 | 配对 | 在线连接 | 控制端会收到 | 还原办法 |
|---|---|---|---|---|
| `level <设备> <级别>` | 保留 | 保留；下一次调用按新级别检查 | 调用超出级别时：`'shell' requires level 'admin', but this device has 'control' ...` | 再执行一次 `level` |
| `level <设备> none` | 保留 | 保留，但每次调用都被拒绝 | `This device no longer has access to this host.`；新连接：`no access level` | `level <设备> <级别>` |
| `revoke <设备>` | 保留，在 `peers` 里标为 `REVOKED` | 切断 | `access revoked by the host ...`（常驻进程运行期间，之后的尝试也是这条） | `unrevoke <设备>` |
| `unrevoke <设备>` | 不变 | - | 只要配对还在，这台设备就可以重新连接 | `revoke` |
| `forget <设备>` | 删除（连同级别） | 切断 | `not paired: ... Pair again with a fresh ticket` | 新票据 |

细节：

- **revoke** 会拒绝这台设备，即使它兑换了新票据也不行。控制端常驻进程会记住自己被撤销了，因为之后的尝试只会碰到
  传输层统一的拒绝。这个记忆只在内存里：常驻进程重启后（空闲 30 分钟会退出），同样的拒绝会显示为 `not paired`。
- 对已撤销的设备执行 **forget**：配对删掉了，拒绝依然有效。命令行会打印以后要用的设备 ID：
  `ai-remote unrevoke eid:<id>`。解除之后，这台设备仍然需要新票据才能重新配对。
- **unrevoke** 在配对还在时用设备名称，配对删掉后用设备 ID（或它的唯一前缀）。控制端的拒绝提示里带有自己的设备 ID，
  就是为了这个用途。
- 控制端上的 **unpair** 只是让控制端忘掉这台被控端；被控端上仍会列出这个控制端，直到你在被控端也执行 `forget`。
- MCP 客户端通常会缓存工具列表。修改级别后，重连客户端（或开一个新会话）才能看到新级别下的工具；
  不论哪种情况，调用都会按当前级别检查。

### 6.1 Bypass：完全信任某台控制端

只从自己的设备操作的电脑（比如测试机）上，审批和级别可能反而碍事。**Bypass** 让你完全信任选定的控制端：

```sh
ai-remote bypass                       # 列出已配对的控制端，开启的标 BYPASS
ai-remote bypass on my-laptop          # 直到你关闭
ai-remote bypass on --all --for 2h     # 所有能连接的控制端，2 小时后自动结束
ai-remote bypass off my-laptop         # 或 --all
ai-remote bypass default on            # 所有已配对的控制端，包括以后配对的
ai-remote bypass default off
```

**默认信任。** 只有你自己（和你的 Agent）会操作的电脑可以一开始就信任所有控制端：`ai-remote bypass default on`
覆盖每一台已配对的控制端，以后配对的也算，不用逐台设置。发现不对劲时，`ai-remote bypass off <设备>` 把那一台
从默认里拿掉（`ai-remote bypass` 里显示为“已关闭”），`level <设备> none`、`revoke` 和控制端自己降级也一样；
`bypass on <设备>` 把它放回去。`forget` 会清掉关于那台设备的一切，重新配对后从默认开始。`bypass default off`
回到只信任逐台打开的控制端。默认开关保存在同一个文件里，写审计（`kind: "bypass"`，带 `default`），开着时
`doctor` 会警告。

那台电脑上的桌面应用在 **设置 → 本设备 → 完全信任的控制端** 里做同样的事：每台已配对的控制端是 *先问我*（默认）
或 *全部放开*，选后者时会先请你确认。

控制端处于 bypass 时，它的调用（以及通过它工作的每个 Agent 的调用）：

- 一律按 `admin` 检查，不管它的级别，所以能看到并使用全部工具，包括文件传输；
- 不等审批，也不受 `deny` 规则（以及挂载工具上的 `approve = "deny"`）限制；
- 照样写审计日志，记录上带 `"bypass": true`。

缓存了工具列表的 MCP 客户端要重连后才能看到新增的工具，和修改级别后一样。

仍然有效的：设备必须仍有配对和某个级别（`level <设备> none`、`revoke`、`forget` 会切断它，并同时结束它的 bypass；
控制端自己降低级别也会结束 bypass）；人仍然可以接管或暂停 Agent（9.3 节）。控制端能看到自己处于 bypass
（`ai-remote access`、`ai-remote info`、`system_info.you.bypass`；桌面应用在电脑页顶部和“访问权限”标签里标出“全部放开”，
iPhone 应用在会话的权限标签里显示，并且不论级别都可以接管），只要有控制端处于 bypass，
`ai-remote doctor` 就会给出警告。

Bypass 只能在这台电脑上、经被控端的本地控制通道设置，控制端无法请求，所以 Agent 不能经 ai-remote 给自己开。
它保存在 `<配置目录>/host/bypass.json`，重启后仍然有效；每次改动都写一条审计记录（`kind: "bypass"`）。
已经能以被控端用户身份运行 `shell` 的 Agent 仍然可以改这个文件，就像能改被控端的任何配置一样
（见 [approvals.zh-CN.md](approvals.zh-CN.md) 的威胁模型）。

## 7. 被控端拒绝了怎么办

拒绝提示同时写给 Agent 和你看：每条都说明发生了什么、该怎么办。Agent 会被告知不要自动重试被拒绝或被否决的调用。

### 7.1 连接被拒

控制端无法与被控端建立会话时报告的内容（提示内容有删节）：

| 提示开头 | 含义 | 怎么办 |
|---|---|---|
| `access revoked by the host` | 机主撤销了这个控制端 | 在被控端执行 `ai-remote unrevoke <本控制端>`；如果配对也删了，重新配对 |
| `not paired` | 被控端不认识这个控制端（从未配对、已被 forget，或在本常驻进程启动前就已被撤销） | 用新票据重新配对；如果被撤销过，机主还要先 `unrevoke` |
| `no access level` | 已配对，但被控端没有授予任何级别 | 在被控端执行 `ai-remote level <本控制端> control` |
| `host offline or unreachable` | 没有响应，后面附原始原因 | 启动被控端，确认机器在线，见第 13 节 |
| `the host closed the connection` | 被控端停止或重启了 | 稍后再试 |
| `this controller closed the connection` | 这台被控端在本机被 unpair 了，或常驻进程正在退出 | 重新配对，或重试 |
| `this controller has revoked that host` | 本机拒绝了那台被控端 | - |
| `connection failed:` | 其他情况，附原始原因 | 看原因；确认两端版本一致 |

需要机主处理时，提示末尾会带上 `This controller's device id: eid:...`，机主据此在 `ai-remote peers` 里找到它
（或者用于 `unrevoke`）。

只有人能解除的拒绝只报告一次，不会循环重拨；下一次尝试会重新拨号，所以被控端上的修复能及时生效。

如果调用执行期间连接断开，调用不会重发。提示末尾会写明
`The call's outcome is unknown: it may have run on the host. Check the host's state before repeating it`。

### 7.2 闸门拒绝调用

| 提示 | 原因 | 怎么办 |
|---|---|---|
| `'<tool>' requires level '<x>', but this device has '<y>'` | 工具需要更高的级别 | 如果合适，机主用 `ai-remote level` 提升级别 |
| `This device no longer has access to this host.` | 级别被设成了 `none`（或配对没了） | 机主重新授予级别 |
| `The host's policy does not allow '<tool>'.` | 命中 `deny` 规则，或工具设了 `approve = "deny"` | 机主修改 `ai-remote.toml` |
| `The host owner declined '<tool>'.` | 机主点了“否” | 问问用户想怎么做 |
| `Nobody approved it on the host in time for '<tool>'.` | 超时没人回答 | 请用户守在被控端旁边，或加一条规则 |
| `No one can approve actions on the host right now (desktop locked or unattended), so it refused '<tool>'.` | 审批框弹不出来 | 解锁桌面，或加一条规则（第 9 节） |
| `cancelled: the session ended` / `cancelled by the controller` | 会话结束，或客户端取消了调用 | - |
| `unknown tool '<name>'` | 被控端没有这个工具（属于协议错误，不记审计） | 检查那台被控端的工具列表 |

### 7.3 工具常见的拒绝

| 提示包含 | 原因 | 怎么办 |
|---|---|---|
| `the desktop is locked or showing a security prompt, which this host cannot reach` | 锁屏、登录界面、UAC 提示，且被控端以用户身份运行 | 解锁；或把被控端装成服务（`host install --service`，第 3.2 节），服务能到达安全桌面 |
| `runs in the services session (session 0)` | 被控端被手动在 0 号会话里启动（服务自己拉起的被控端在控制台会话里） | 改在用户会话里运行（`host install`，带不带 `--service` 都行） |
| `an ai-remote approval prompt is open on the host` | 有审批框在等回答，输入暂停 | 等机主回答后重试 |
| `the input would go to one of ai-remote's own windows` | 目标是审批框或被控端所在的终端 | 换个窗口操作 |
| `running with higher privileges` | 目标窗口属于以管理员身份运行的程序 | 以管理员权限运行被控端（由管理员执行 `host install`） |
| `is refused: it locks the workstation`（及类似提示） | 锁屏或注销的组合键 | 不要发送，交给机主决定 |
| `is refused: it opens the secure attention screen` | 被控端不是服务时按 `ctrl+alt+del` | 把被控端装成服务（`host install --service`），之后由服务代发 |
| `mount '<m>' is not running: it failed N times in a row` | 挂载的 server 反复崩溃 | 查看 `host/mounts/<m>.log`；见 [tools.zh-CN.md](tools.zh-CN.md#75-生命周期崩溃与重启) |
| `refusing to start ...: its SHA-256 is ...` | 挂载 server 的可执行文件变了 | 如果是有意更新，重新添加这个挂载 |

各工具特有的错误列在 [tools.zh-CN.md](tools.zh-CN.md) 对应工具的说明里。

## 8. 体检（doctor）

```sh
ai-remote doctor [--json]
```

会向正在运行的被控端索取检查结果，所以看到的是被控端自己的会话、令牌和网络。没有被控端在运行时，
命令行就在你的终端里自己检查，并跳过网络检查。同一份报告也是 `doctor` 工具的结果（Agent 会被告知在任务开始前
先跑一次，遇到 `fail` 就停下），以及 `host install` 打印的自检。

每项检查的结果是 `ok`、`warn`、`fail` 或 `skip`（不适用，或在当前位置无法检查），凡不是 `ok` 的都附修复办法。
总评取最差的一项；总评为 `fail` 时，`ai-remote doctor` 以退出码 1 退出。

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

| 检查项 | 何时为 ok | 不是 ok 意味着 | 修复办法 |
|---|---|---|---|
| `session` | 处于交互式 Windows 会话（不是 0 号会话） | **fail**：服务会话没有桌面，截图全黑，输入没有去处 | 从用户会话中启动被控端；`host install` 就是这样注册的（服务也是：它拉起的被控端在控制台会话里） |
| `console` | 会话连接在物理控制台上 | **warn**：这是远程桌面会话；断开后截屏和输入都会停止，控制台保持锁定 | 不要直接断开，而是把会话交还给控制台：在管理员命令行执行 `tscon <会话> /dest:console` |
| `desktop` | 输入桌面是 `Default`（未锁定） | **fail**：已锁定或正显示登录界面、UAC 提示，而被控端到不了那里；**warn**：同样状态但被控端是服务，能看能操作（审批弹窗显示不了） | 解锁；无人值守的机器关闭自动锁屏，或装成服务（`host install --service`） |
| `elevation` | 被控端以管理员权限运行 | **warn**：以管理员身份运行的程序的窗口会忽略它的输入 | 以最高权限运行（由管理员执行 `host install`） |
| `dpi` | 逐显示器 DPI 感知（v2） | **fail**：在有缩放的显示器上点击位置会偏 | 去掉可执行文件上的高 DPI 兼容性覆盖设置（属性 > 兼容性） |
| `network` | `ai-remote.toml` 能读取；说明被控端在连接之外还往网络上发什么（局域网发现、路由器端口映射） | **warn**：文件读不了 | 修好这个文件（第 11 节） |
| `p2p` | UDP 能出去，NAT 不是对称型 | **warn**：UDP 被阻断（所有连接都走中继）、对称型 NAT（直连常常失败）或需要网页登录的网络 | 放行出站 UDP、路由器支持 UPnP 或 NAT-PMP 时打开路由器端口映射（设置 > 网络）、用 IPv6，或先完成网络登录 |
| `relay` | 已连上主中继（home relay） | **warn**：没有使用中继，或一个都连不上；其他网络里的控制端可能连不进来 | 检查网络连接、出站防火墙或代理（中继走 HTTPS），或检查 `[network] relays` |
| `lan` | （仅限局域网模式，代替 `relay`；`p2p` 跳过）被控端在固定端口上监听；列出控制端应使用的地址 | **warn**：固定端口被占用，被控端改用了别的端口，记着固定端口的控制端连不上它 | 释放这个端口或另设 `[network] port`，然后重启被控端 |
| `approval` | 能弹出审批框 | **warn**：需要审批的操作都会被拒绝 | 解锁桌面，或用 `[[rule]]` 放行可信的工具 |
| `bypass` | 没有控制端处于 bypass | **warn**：列出的控制端（默认开着时是所有已配对的控制端）可以不经审批使用全部工具（6.1 节） | 不再需要时 `ai-remote bypass off <设备>`（或 `--all`、`bypass default off`） |
| `firewall` | 有入站允许规则覆盖被控端程序的 UDP，或防火墙已关闭 | **fail**：有阻止规则作用于该程序（流量会退回中继）；**warn**：没有允许规则（Windows 会在第一次连接时弹提示），或者有针对所有程序入站 UDP 的阻止规则 | 在管理员终端执行 `host install`；其他程序的规则在 `wf.msc` 里检查 |
| `autostart` | 任务（或 LaunchAgent）会用本配置目录启动本次安装的后台被控端 | **warn**：没有安装，或者运行的是别的程序 | （再）执行一次 `ai-remote host install` |
| `audit` | 审计日志能以追加方式打开 | **fail**：操作不会被记录 | 修正配置目录的权限，或换一个 `--home` |
| `disk` | 配置目录所在磁盘剩余 5 GiB 以上 | 低于 5 GiB 为 **warn**；低于 0.5 GiB 为 **fail**（日志和审计日志可能写不进去） | 清理磁盘空间 |

防火墙检查看的是运行被控端的那个程序：有被控端在运行时，看它的可执行文件；没有时，看自启项将要运行的那个。

## 9. 审批

调用的动作为 `ask` 时需要审批。这是 `shell`、`jobs_start`、`files_mkdir`、`files_delete`、`files_push`、`files_pull` 的默认动作，也是两类挂载工具的默认动作：
没有 `approve` 设置的 admin 级挂载工具，以及设置了 `approve = "ask"` 的挂载工具。此外，任何策略规则写了 `ask`
时同样需要审批（见 [tools.zh-CN.md](tools.zh-CN.md#8-策略规则)）。处于 bypass 的控制端的调用从不需要审批（6.1 节）。

能回答的人有两类，先到的回答生效：被控端前的人，在它桌面上的对话框里回答（9.1）；机主，在被控端接受了其审批密钥的
已配对设备上回答（9.2）。

### 9.1 在被控端审批

除非设置了 `approval_prompt = false`，被控端会在自己的桌面上弹出一个原生警告对话框，标题为“ai-remote: approve an action”：

```
"laptop" wants to run: shell

Why: Check free disk space before the update

Details:
{"command":"Get-PSDrive C","intent":"Check free disk space before the update"}

Allow this once? (No answer within 60 s counts as No.)
```

- **是**：只执行这一次调用。没有“总是允许”按钮，需要的话用策略规则。
- **否**，或在 `approval_timeout_secs`（默认 60，最少 5）内没有回答，都会拒绝这次调用，并告诉 Agent 是哪一种情况。
  弹不出对话框时（桌面锁定、安全桌面、没有交互会话），只有审批设备能回答；一台都没有就立即拒绝。
  超时后，或者设备先回答了，对话框可能还留在屏幕上，但它的回答会被忽略。
- 调用方能控制的文本会被安全地显示：控制字符会转义，理由截断到 600 个字符，参数预览约 400 个字符。
  机密值（环境变量的值、标准输入、剪贴板文本）只显示长度，比如 `<hidden: 12 chars>`。
- 只要有审批框在等回答，所有控制端的所有鼠标和键盘调用都会被拒绝，而且输入永远不能对准被控端自己的窗口，
  所以控制端没法自己批准自己的请求。
- 无人值守的机器上没人能在被控端回答。可以改在设备上审批（9.2），或者只给这类设备它真正需要的工具，
  或者用规则为特定设备放行特定工具（`action = "allow"` 会跳过审批）。

### 9.2 在已配对设备上审批

机主可以在已配对设备上允许或拒绝。允许需要这台设备的审批密钥，它由操作系统保管、受 Windows Hello 保护
（macOS 和 iPhone 应用上是 Touch ID 和 Face ID），所以跑在同一台设备上的 Agent 批准不了自己的请求。拒绝不需要密钥。
只需设置一次：

```bash
# 在控制端设备上：生成密钥（Windows Hello 会请你确认），并提交给被控端
ai-remote approver --enroll office-pc
```

被控端前的人在提示里确认这把新密钥。如果那边桌面不可用，就在设备上用 `ai-remote approver --create` 打印密钥，再到被控端的终端里
添加：`ai-remote approvers add <设备> <算法>:<密钥>`。之后：

```bash
ai-remote approvals                                  # 正在等待的审批及其 id
ai-remote approve 3f9c2a                             # 通过 Windows Hello 后，调用开始执行
ai-remote deny 3f9c2a --note "use the recycle bin"   # Agent 会收到这句话
```

没人坐着的机器可以设置 `approval_prompt = false`，并调长 `approval_timeout_secs`。设计、签名的消息和威胁模型见
[approvals.zh-CN.md](approvals.zh-CN.md)。

### 9.3 接管、暂停和给 Agent 留言

Agent 和人在同一台电脑上轮流操作。在级别为 `control` 或以上的已配对设备上：

```bash
ai-remote take-over             # 由你操作；取消执行中/排队的 Agent 调用，拒绝后续调用
ai-remote take-over --let-watch # 同上，但 Agent 还可以继续看（只限 observe 级工具）
ai-remote hand-back --note "I fixed the chart title"
ai-remote pause                 # 暂停，谁都不操作；任何已配对设备都可以暂停
ai-remote resume
ai-remote tell "Use the D: drive for the export"
```

- 你操作期间只有你自己的调用会执行：桌面应用替你发出的调用，或者 `ai-remote call ... --person`。你键入的内容在日志里只记录长度。
- 桌面应用里接管后，鼠标和键盘走一条实时输入流（`input/1`）：移动、按下、松开、滚轮、按键按键名逐个即时发送，
  审计只记这条流的开始、结束和被拒（`input_start` / `input_end` 带事件计数和结束原因 / `input_refused`）。
  只有正在接管的那台设备能开这条流；每秒复查一次，交还、暂停、别的设备接管、级别下调、bypass 结束或策略变化后
  1 秒内结束，结束前被控端松开所有按住的键和按钮；控制端 3 秒没有任何消息（应用每秒发心跳）也会松开。被控端策略对
  `mouse` 或 `keyboard` 是 `deny` 或 `ask` 时这条流打不开，应用退回逐条工具调用（那条路会问机主）；旧版被控端也是
  这样。实时流里 Mac 操作 Windows 时 ⌘ 当 Ctrl 用，Windows 操作 Mac 时 Ctrl 当 ⌘ 用（逐条工具调用的退回路径里
  ⌘ 仍是 win 键）；按住 Ctrl / ⌥ / ⌘ 时字母和数字按键位发送，不受本机布局影响；被控端布局打不出的字符按文本输入；
  锁屏、注销的组合键和工具一样被拒。
- 接管期间剪贴板文本双向同步，只同步之后的变化（接管本身不会改动任何一边的剪贴板）：被控端复制的文本到本机，
  本机复制的文本到被控端（应用在前台时每秒、以及切回应用时读一次本机剪贴板，单次最多 1 MB）。被控端读和写分别受
  `clipboard_read` / `clipboard_write` 策略约束（`allow` 才同步）；被控端锁屏或处于安全桌面时不动剪贴板。
  被控端剪贴板里有文件时会话顶部出现提示，可一键保存到本机"下载"；本机剪贴板里有文件时可一键发到被控端桌面；
  把文件拖到会话画面上（不必先接管）也会发到被控端桌面（`~/Desktop`，即登录用户实际看到的桌面），同名文件不覆盖、
  自动加 `-1` 等后缀。这些传输和命令行 `push` / `pull` 走同一套票据、策略和审批。
- 接管或暂停会使已有 Agent 调用的控制授权失效，包括等待审批和排队的输入。交还或恢复只允许新调用，不会继续旧动作。
  输入线程在每个事件前检查取消，包括拖动过程中的指针移动；停止时释放已按下的键和鼠标按钮。
  已发送给系统的事件无法撤回，因此发出替代动作前应重新查看屏幕。`--let-watch` 允许接管期间继续执行 observe 级调用。
- 输入最多一批执行、一批等待；更多调用立即返回 `input busy`。应等待现有调用结束并核对现场后再发送。
  等待使用异步锁，避免占用截图和原生审批框需要的工作线程。
- 交还控制以及每条 `tell` 都会排队一条留言，附在 Agent 下一次调用的结果里（“Messages from the owner while you worked: ...”），
  让 Agent 知道屏幕可能变了。
- 每次切换都是一条审计记录（`driver`：`agent`、`person` 或 `paused`，谁操作的，附言），每个会话也会记录它属于 Agent 还是人
  （`session_start` 里的 `client`）。
- 轮流操作是为了协调人和 Agent，不是安全边界（同一台设备上的 Agent 也能发出标记为“人”的调用）。真正限制 Agent 的是
  权限级别、策略和审批。

**在被控端本机暂停。** 机主也可以在被控端的桌面端（托盘菜单或“设置 → 本设备”）暂停。这种暂停比控制端的更强：智能体的调用
*和*接管者的调用都会被拒绝，任何已配对设备都不能恢复或接管，直到机主在那台电脑上恢复（控制端执行 `ai-remote resume` 会被拒绝并
说明原因）；画面仍可观看。恢复后，暂停前是谁在操作就交还给谁。审计日志里记为带 `"origin": "owner"` 的 `driver` 行，控制端的时间线
显示“<被控端> 前的人暂停了操作”。

## 10. 审计日志

被控端上的每一次工具调用都会追加到 `<配置目录>/host/audit.jsonl`，每行一个 JSON 对象。
执行了的调用有两条记录：执行前的 `decision` 和执行后的 `outcome`。被拒绝的调用只有 `decision` 记录。

| 字段 | 含义 |
|---|---|
| `ts` | Unix 纪元以来的毫秒数 |
| `kind` | `decision` 或 `outcome` |
| `session` | MCP 会话 ID，8 位十六进制 |
| `peer` | 被控端给这个控制端起的名字 |
| `principal` | 控制端的设备 ID，`eid:<hex>` |
| `tool` | 工具名（挂载工具带前缀） |
| `decision` | `allow`、`approved`、`denied`、`timeout`、`unavailable`（无法弹出审批框）、`level`（因级别被拒）、`policy`（被规则拒绝） |
| `intent` | 调用方给出的理由（如有） |
| `args_hash` | 参数 JSON 的 BLAKE3 哈希：不保存参数值也能找出相同的调用 |
| `args_preview` | 仅 `decision` 记录有：给机主看的参数，最多约 400 个字符（保留首尾），机密只记长度 |
| `outcome` | `ok`、`error` 或 `cancelled` |
| `duration_ms` | 调用耗时 |
| `result_bytes` | 结果内容序列化为 JSON 后的大小，按结果预算处理之后计算 |
| `error` | 错误文本；超过 300 个字符时只保留首尾各 140 个 |
| `bypass` | 调用方是处于 bypass 的控制端时为 `true`（6.1 节）：按 admin 检查，不走策略和审批 |

```json
{"ts":1791021600123,"kind":"decision","session":"5f2c19ab","peer":"laptop","principal":"eid:3b7e...","tool":"shell","decision":"approved","intent":"Check free disk space","args_hash":"9f2d...","args_preview":"{\"command\":\"Get-PSDrive C\",\"intent\":\"Check free disk space\"}"}
{"ts":1791021601412,"kind":"outcome","session":"5f2c19ab","peer":"laptop","principal":"eid:3b7e...","tool":"shell","intent":"Check free disk space","args_hash":"9f2d...","outcome":"ok","duration_ms":1289,"result_bytes":512}
```

ai-remote 不会轮转或清理审计日志，文件变大时请自行归档。审计日志写不进去时，doctor 的 `audit` 检查会失败。
后台任务在 `host/jobs/` 里另有自己的记录（命令、所属设备、理由、时间、退出码；从不记录环境变量）。

## 11. 被控端配置文件

`<配置目录>/host/ai-remote.toml` 是可选的；没有它时一切都用默认值。被控端只在启动时读取它，所以修改后要重启被控端
（先 `ai-remote stop-host`，再重新启动）。文件解析失败时被控端无法启动：错误显示在终端里，后台被控端则写在
`host.log` 里（Windows 上守护进程会按 3.3 节的退避策略不断重试，所以文件修好后最多 5 分钟就会生效）。

```toml
[host]
name = "office-pc"           # 显示给控制端的名称；默认用机器名
approval_timeout_secs = 60   # 审批等待多久；最少 5
approval_prompt = true       # false：本机不弹对话框，只在设备上审批（9.2 节）

[network]
relays = []                  # 留空：使用默认的公共中继（第 12 节）
relay_fallback_n0 = true     # 配了 relays 时，保留 n0 公共中继作兜底
proxy = ""                   # 留空：自动识别 HTTP 代理；"none"：不用代理；或填代理地址
lan_discovery = "off"        # "on"、"resolve"（只查找不广播）或 "off"（第 12 节）
port_mapping = false         # 请路由器开端口（UPnP）；桌面端 设置 > 网络 写的就是它
quiet = false                # true：上面两项不管怎么写都保持关闭（第 12 节）
mode = "auto"                # "lan"：只在局域网里直连，不用中继（第 12 节）

[[rule]]                     # 策略规则，第一条命中的生效
tool = "shell"
action = "ask"

[[mount]]                    # 挂载的 MCP server
name = "ui"
command = "C:/tools/cua-driver/cua-driver.exe"
args = ["mcp"]
[mount.tools]
click = "control"
```

`[[rule]]` 的说明见 [tools.zh-CN.md](tools.zh-CN.md#8-策略规则)，`[[mount]]` 见
[tools.zh-CN.md](tools.zh-CN.md#7-挂载-mcp-server)。只有 `[[mount]]` 表及其工具条目会拒绝未知的键；
其他地方拼错的键（比如规则里把 `principal` 写成 `principle`）会被直接忽略，没有任何警告，所以请仔细核对拼写。
`ai-remote mount ...` 修改这个文件时会保留你的注释和排版。

## 12. 跨网络连接

配对后的设备不需要端口映射或 VPN 就能找到并连上对方。下面这些方式默认开启，只有两项例外：路由器端口映射和局域网发现
会在连接之外往网络上发包，默认关闭，需要时再打开。

| 方式 | 作用 | 默认 |
|---|---|---|
| 直连（P2P） | UDP 打洞，IPv4 和 IPv6 | 开 |
| 路由器端口映射 | 请路由器开放一个端口（UPnP、NAT-PMP、PCP），用于只靠打洞穿不过的路由器 | **关**；在桌面端 设置 > 网络 打开，或写 `port_mapping = true` |
| 局域网 | 用 mDNS 在同一网络里找到对方，不连外网也行 | **关**；`lan_discovery = "on"` |
| 仅限局域网 | 不用中继也不用地址查询：设备按记下的地址直连，网络上只有设备之间的连接 | **关**；桌面端 设置 > 网络，或写 `mode = "lan"` |
| 中继 | 打不通直连时由加密中继经 HTTPS（443 端口）转发；一旦打通直连就自动切过去 | 开 |
| HTTP 代理 | 只能经代理出网的网络里，中继连接走代理 | 开；`proxy = "none"` 关闭 |
| 中继兜底 | 配了自建中继时，保留 n0 公共中继作兜底 | 开；`relay_fallback_n0 = false` 关闭 |
| 防火墙检查 | 控制端发现系统防火墙阻止了自己（点掉“允许访问”弹窗会留下阻止规则）时，桌面端提供“在防火墙中允许”（一次管理员确认） | 开 |
| 重试 | 控制端连不上时，1 秒和 3 秒后各重拨一次（刚切换网络时常常能连上） | 开 |

走中继仍是端到端加密，但延迟更高；n0 的免费公共中继还有限速。走中继期间各端都会提示“P2P 直连失败，经 n0 公共中继转发”。

**配置。** 被控端读 `<profile>/host/ai-remote.toml` 里的 `[network]`（第 11 节）；控制端读
`<profile>/controller/ai-remote.toml` 里同样的 `[network]` 段。都在进程启动时读取。

```toml
[network]
relays = ["https://relay1.example.org", "https://relay2.example.org"]
relay_fallback_n0 = true
proxy = ""            # 留空或不写：自动识别；"none"：不用；或者如 "http://proxy.corp:3128"
lan_discovery = "off" # "on"：在局域网里广播本机；"resolve"：只查找别人
port_mapping = false  # 请路由器开端口；桌面端 设置 > 网络 写的就是它
port = 0              # 固定 UDP 端口，用于在路由器上做端口转发；0 或不写：随机
quiet = false         # true：lan_discovery 和 port_mapping 不管怎么写都保持关闭
mode = "auto"         # "lan"：只在局域网里直连（不用中继，不用地址查询）
address_lookup = []   # 留空：用 n0 的地址查询服务；或填自建的 pkarr 服务器
```

- **中继。** 列表为空表示用 n0 的公共中继。列出自建的 [iroh-relay](https://github.com/n0-computer/iroh) 服务器后，
  默认仍保留 n0 的中继作兜底，设 `relay_fallback_n0 = false` 可去掉。配对票据里记录着生成时被控端所用的中继，
  改了中继要重新生成票据。在被控端执行 `ai-remote doctor` 可以看到是否连上了中继（`relay` 检查）。
- **在 app 里设置。** 设置 > 网络 里可以在 **自动**（推荐，在哪里都能连）和 **仅限局域网** 之间选择，另有两个默认关闭的开关：
  **在局域网里自动发现设备**（`lan_discovery`）和 **让路由器开放端口（UPnP）**（`port_mapping`；仅限局域网时用不到，会隐藏）。
  选了仅限局域网，还会显示这台电脑的地址（可复制）和这台设备要连接的每台电脑的地址（可修改）。修改会同时写入这台电脑的
  被控端和控制端配置；控制端立即生效（正在进行的会话会短暂重连），这台电脑上运行的被控端下次启动时生效。在终端里用
  `ai-remote network` 效果相同。
- **iPhone / iPad。** 电脑列表左上角的 **网络** 打开同样的页面，改的是手机自己的控制端配置（`<应用数据>/controller/ai-remote.toml`）：
  自动 / 仅限局域网，以及两个默认关闭的开关。手机上的局域网发现开关写的是 `"resolve"`：只查找电脑，从不广播手机自己。
  选了仅限局域网，会列出每台已配对电脑的地址并可修改；这时配对码要靠局域网发现，配对页会说明并提供完整票据。修改后手机的
  连接核心立即按新设置重启。电脑那边也要选仅限局域网，它的地址才不会变。
- **代理。** 按这个顺序识别：`proxy` 设置、环境变量 `HTTPS_PROXY` / `ALL_PROXY` / `HTTP_PROXY`、系统设置
  （Windows：当前登录用户的 Internet 设置；macOS：`scutil --proxy`）。不解析代理自动配置（PAC）脚本，这种网络请直接填
  `proxy`。只支持 HTTP(S) 代理，不支持 SOCKS。只有中继连接走代理：直连是 UDP；对方换了地址时的地址查询仍直接访问 n0 的
  DNS 服务。被控端日志里会写明用的是哪个代理。
- **局域网。** 默认关闭。`"on"` 会在局域网里广播本机（设备 ID 和地址，包括公网地址，大约每秒一次，同网段所有设备都收得到），
  让别人完全不连外网也能找到它；`"resolve"` 只查找别人、不广播自己，但仍然大约每秒发一次查询。有外网时，同一网络里的设备
  不靠它也能直连：发布出去的地址里本来就包含内网地址。在仅限局域网模式下打开它，对方换了 IP 也能自动重新找到，不用手动改地址。
  app 里的开关写的是 `"on"` 或 `"off"`。
- **路由器端口映射。** 默认关闭。在桌面端 设置 > 网络 里打开：它会把 `port_mapping = true` 写进这台电脑的被控端和控制端配置；
  控制端立即生效（正在进行的会话会短暂重连），这台电脑上运行的被控端在下次启动时生效。`doctor` 的 `p2p` 报 warn（对称型 NAT）
  且路由器支持 UPnP、NAT-PMP 或 PCP 时值得打开。没有映射期间它每 20 到 26 秒探测一次网关（UPnP 搜索组播和 NAT-PMP/PCP 请求），
  受管理的网络可能会把这当成可疑行为。
- **仅限局域网。** `mode = "lan"`，或桌面端 设置 > 网络 里选"仅限局域网"（会同时写入这台电脑的被控端和控制端配置）。不用中继、
  不用地址查询、不做网络探测：网络上只有设备之间的流量。被控端在 UDP 47800 端口（或它的 `port`）监听；控制端按为每台被控端
  记下的地址去连：配对票据里的被控端地址，之后每次连接都会自动更新。被控端地址变了（换了 IP，或者切到仅限局域网之前用的是随机
  端口），就在控制端上改：桌面端 设置 > 网络 里改，或执行 `ai-remote hosts address <被控端> 192.168.1.20`（不写端口就是 47800）。
  在被控端执行 `doctor` 可以看到该用哪些地址（`lan` 检查）。用配对票据配对照常可用；配对码要靠局域网发现才能被找到，没打开“在局域网里自动发现设备”时，被控端只给出票据并说明原因。不在这个网络里的设备连不上，所以只在
  两端处于同一网络（公司、家里）时在两端都打开，并给被控端固定 IP（在路由器上做 DHCP 保留）。
- **固定端口。** 在打洞很难成功的 NAT 后面（`doctor` 的 `p2p` 为 warn），可以在路由器上把一个 UDP 端口转发到被控端，
  并把 `port` 设成它，控制端就能直连。被控端启动时这个端口如果被占用，会改用随机端口并在日志里说明。
- **安静模式。** `quiet = true` 把局域网发现和路由器端口映射锁定为关闭，不管 `lan_discovery`、`port_mapping` 或桌面端的设置
  怎么写。用在公司这类受管理的网络里：在那里，连接本身以外的流量容易引人注意。在被控端执行 `ai-remote doctor` 可以看到
  打开了哪些（`network` 检查）。无论哪种模式，名称查询都只走系统 DNS：查询失败就失败，不会转去问 Cloudflare、Google 或
  Quad9（只有系统完全没有配置 DNS 时才用公共 DNS）。配对过的设备照样能连上：通过地址查询服务找到对方，网络允许时打洞直连，
  不行就走中继。网络的 DNS 解析不了中继和地址查询服务的域名时完全连不上（这时请配上这个网络能解析的自建中继和地址查询服务器）。
  锁定时 app 里两个开关都显示为关闭且不可操作，`ai-remote network discovery on` / `upnp on` 会拒绝并说明原因。
  屏幕上显示配对码期间，被控端另起一个短时的连接端点来应答它，网络行为和上面一致（中继、地址查询服务器、端口映射、局域网发现）。
- **地址查询。** 每台设备每 5 分钟、以及地址变化时，把自己当前的地址发布到 n0 的地址查询服务（`dns.iroh.link`），
  拨号前也在那里查对方的地址。设了 `address_lookup = ["https://dns.example.org/pkarr"]` 后，发布和查询都改用你自建的
  [iroh-dns-server](https://github.com/n0-computer/iroh)。需要互相找到的设备必须配同一组服务器。再加上自建中继和
  `relay_fallback_n0 = false`，就不会有任何流量到 n0。

### 网络上都有哪些流量

依据是代码（iroh 1.3）。2026-10-21 在公司网络里按进程抓包（见 AGENTS.md）：两项都关闭时，ai-remote 发往 5353、1900、5351
的包为 0。最后一行是连接本身。

| 流量 | 发往 | 什么时候 | 默认 |
|---|---|---|---|
| 中继连接 | 中继（n0 的是 `use1-1`、`usw1-1`、`euc1-1`、`aps1-1` `.relay.n0.iroh.link`），TCP 443 上的 HTTPS | 运行期间一直保持，每 15 秒一次 ping | 开 |
| 网络探测 | 中继：443 上的 HTTPS、UDP 7842 上的 QUIC 地址发现、80 端口 HTTP 的网络登录页检测 | 每 20 到 26 秒一次，每 5 分钟一次完整探测，网络变化时也探测 | 开 |
| 地址发布与查询 | `dns.iroh.link`（或 `address_lookup`）的 HTTPS 443，以及经系统发出的 DNS 查询 | 每 5 分钟和地址变化时发布；拨号前查询 | 开 |
| 路由器端口映射 | 网关：UPnP SSDP 组播到 239.255.255.250:1900，NAT-PMP / PCP 到 UDP 5351 | 没有映射时每 20 到 26 秒一次（多数公司网络没有映射服务） | 关（设置 > 网络） |
| 局域网发现 | mDNS 组播到 224.0.0.251:5353，同网段所有设备都能看到；广播时会通告本机设备 ID 和地址（包括公网地址） | 大约每秒一次 | 关（`lan_discovery`） |
| 公共 DNS 兜底 | 1.1.1.1、8.8.8.8、9.9.9.9（UDP 53、HTTPS 443） | 只在系统完全没有配置 DNS 时 | 仅此时 |
| 连接 | 对方设备（UDP 打洞）或中继 | 配对设备连着的时候；每 5 秒一次保活；流量大头是推流画面 | 开 |

除此之外没有别的：不扫描端口或地址，不检查更新，不上报遥测。桌面端界面只和自己的进程通信。

## 13. 故障排查

**截图全黑或只有一种颜色。** 图片整张是黑色或单一纯色时，`screenshot` 的结果里会带 `warning`。常见原因：
桌面已锁定，或正在显示登录界面、UAC 提示；显示器关闭或休眠；有独占全屏或带 DRM 保护的程序；
或者被控端没有运行在已登录用户的交互会话里。在被控端执行 `ai-remote doctor`（看 `session`、`console`、`desktop`），
并检查 `system_info.session`。

**桌面已锁定。** 桌面锁定或显示安全提示期间，截屏、切换窗口、鼠标、键盘、剪贴板以及需要桌面的挂载工具都会拒绝
并说明原因，所有审批也都会被拒绝。解锁即可。无人值守的测试机请关闭自动锁屏（屏保锁定、睡眠唤醒后锁定、动态锁），
否则一锁屏，被控端就用不了了。

**远程桌面（RDP）。** 在 RDP 会话里运行的被控端，会话连着时一切正常。RDP 客户端一断开，会话就失去显示
（截屏和输入都停止），物理控制台则停在锁屏界面。断开之前，在被控端的管理员命令行里把会话交还给控制台：
`tscon <会话 ID> /dest:console`（`console` 体检项会打印带正确 ID 的命令）。

**以管理员身份运行的窗口。** Windows 会悄悄丢掉发给更高权限程序（安装程序、管理工具、经 UAC 提升的程序）的输入。
被控端会拒绝这类输入并说明原因，而不是假装成功。解决办法是以管理员权限运行被控端：在管理员的提升终端里执行
`ai-remote host install`（任务以最高权限运行）。UAC 的同意提示本身在安全桌面上，完全无法操作。

**走了中继而不是直连。** `list_hosts`（在 MCP 客户端里）和 `ai-remote hosts --json` 会给出在线连接的 `path`：
`direct`、`relay` 或 `unknown`，以及 `relay`（哪个中继）和 `relay_n0`（是否为 n0 的免费公共中继，有限流）。
`ai-remote hosts` 会打印线路；桌面端会话工具栏、iPhone/iPad 会话顶栏在走中继期间会显示“P2P 直连失败 · 经 n0 公共中继转发”。
走中继也能用，仍是端到端加密，只是延迟更高。被控端上的常见原因：有针对被控端程序的防火墙阻止规则
（doctor 的 `firewall` 检查失败；在管理员终端执行 `host install` 会删掉它）；有阻止所有入站 UDP 的规则
（doctor 会警告，请在 `wf.msc` 里检查）；或者所在网络无法完成 UDP 打洞（部分企业网络和移动网络）。

**防火墙提示。** 程序第一次监听时，Windows 会在桌面上询问是否允许。无人值守的机器上没人回答，
而这个没人回答的提示会留下阻止规则，阻止规则又优先于允许规则，于是连接悄悄退回中继。`host install`
会预先添加允许规则，并删掉这类过期的阻止规则；`ai-remote host status` 会按程序列出这些规则。

**`focus_window` 报 `foreground lock`。** Windows 有时不允许把别的程序的窗口切到前台。改用 `mouse` 在窗口里点一下，
或请用户自己切过去。

**连不上被控端。** 在被控端执行 `ai-remote host status`（是否在运行？）和 `ai-remote doctor`（中继、防火墙）。
在控制端，`list_hosts` 或 `ai-remote call system_info` 的报错里带有原始原因。确认两台机器都在线，
并且允许出站 HTTPS 访问中继。

**常驻进程起不来。** `ai-remote mcp` 和控制端命令会报 `the controller agent did not start` 并给出日志路径；
查看 `<配置目录>/controller/controller.log`。在终端里执行 `ai-remote agent` 可以直接看到同样的信息。

**MCP 客户端里看不到新被控端的工具。** 客户端获取工具列表时，那台被控端还没配对或连不上。重连客户端
（比如在设置里重启这个 MCP server）；工具列表是按当时能连上的被控端生成的。

**`a host is already running for this profile`。** 这个配置目录已经有被控端在用（在终端里或在后台）。
`ai-remote host status` 会显示它的 pid 以及是否在后台运行；用 `ai-remote stop-host` 停掉它，或用 `--home`
换一个配置目录。
