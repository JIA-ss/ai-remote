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
12. [自建中继](#12-自建中继)
13. [故障排查](#13-故障排查)

## 1. 命令一览

所有命令都接受 `--home <DIR>`（换一个配置目录，见第 2 节）和 `--json`（输出机器可读的 JSON；`host`、
`host install`、`host uninstall`、`mcp`、`agent`、`stop-agent` 会忽略它）。这两个选项放在子命令前后都可以。
其余用法见 `ai-remote <命令> --help`。

**被控端上：**

| 命令 | 作用 |
|---|---|
| `ai-remote host [--no-pair-code]` | 在当前终端运行被控端，直到按 Ctrl+C 或执行 `stop-host` |
| `ai-remote host install` | 登录时自动在后台启动被控端 |
| `ai-remote host uninstall` | 删除自启项、停止被控端、删除防火墙规则 |
| `ai-remote host status` | 查看自启项、防火墙规则，以及被控端是否在运行 |
| `ai-remote doctor` | 分级体检：这台机器到底能不能被操作？ |
| `ai-remote pair-code [--level L] [--name N]` | 打印一张一次性配对票据 |
| `ai-remote peers` | 已配对的控制端，以及各自的级别和设备 ID；被撤销的标为 `REVOKED` |
| `ai-remote level <设备> <observe\|control\|admin\|none>` | 修改某个控制端的级别 |
| `ai-remote revoke <设备>` / `unrevoke <设备>` | 拒绝某个控制端，或解除拒绝 |
| `ai-remote forget <设备>` | 删除某个控制端的配对 |
| `ai-remote stop-host` | 停止正在运行的被控端 |
| `ai-remote approvers [add\|remove]` | 审批密钥：哪些人可以在已配对设备上审批（见 [approvals.zh-CN.md](approvals.zh-CN.md)） |
| `ai-remote mount list\|add\|add-profile\|remove\|test` | 管理挂载的 MCP server（见 [tools.zh-CN.md](tools.zh-CN.md#7-挂载-mcp-server)） |

**控制端上：**

| 命令 | 作用 |
|---|---|
| `ai-remote pair <票据> [--name N]` | 与被控端配对 |
| `ai-remote hosts` | 已配对的被控端，以及配对时各自授予的级别 |
| `ai-remote unpair <被控端>` | 在本控制端上忘掉某台被控端 |
| `ai-remote call <工具> ['{JSON 参数}'] [--host H]` | 调用一个工具并打印结果 |
| `ai-remote watch [--host H]` | 实时跟踪被控端的活动：先是最近的记录，然后是发生中的每次调用和会话 |
| `ai-remote approvals` / `approve <id>` / `deny <id>` | 列出、允许或拒绝等待审批的调用（见 [approvals.zh-CN.md](approvals.zh-CN.md)） |
| `ai-remote approver [--enroll <被控端>]` | 本设备的审批密钥，或把它提交给被控端 |
| `ai-remote mcp` | 给 MCP 客户端用的 stdio MCP server |
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
它还会打印一张配对票据（级别 `control`）；加 `--no-pair-code` 可以不打印。如果眼下弹不出审批框（桌面已锁定），
它会警告：需要审批的操作都会被拒绝。

用 Ctrl+C 停止，或在另一个终端执行 `ai-remote stop-host`。停止时，所有控制端连接都会以 `host stopping` 为原因关闭。
每个配置目录只能运行一个被控端：再执行一次 `ai-remote host` 会报错 `a host is already running for this profile`。

### 3.2 后台运行：安装与登录自启

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

#### macOS

安装会写一个用户级 LaunchAgent
`~/Library/LaunchAgents/io.github.jia-ss.ai-remote.host.<8 位十六进制>.plist`，并用
`launchctl bootstrap gui/<uid>` 加载。它在登录时运行 `ai-remote-hostw --home <配置目录>`，只在图形（Aqua）
会话中、以交互进程运行；崩溃后 launchd 会重启它，正常退出则不会，所以执行 `ai-remote stop-host` 之后，
它会一直保持停止，直到下次登录。安装不需要管理员权限。`ai-remote-hostw` 第一次截屏或操作时，macOS
会请求“屏幕录制”和“辅助功能”权限，在“系统设置 > 隐私与安全性”里都允许即可。macOS 被控端本身规划在 M1；
这里描述的 LaunchAgent 安装已经实现。

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
- **`stop-host` 之后重新启动：** 注销再登录；或者再执行一次 `ai-remote host install`（没有被控端在运行时它会启动任务）；
  或者自己启动任务：`schtasks /Run /TN "ai-remote\host-<8 位十六进制>"`（任务名见 `ai-remote host status`）。
- **不弹控制台窗口。** 后台被控端没有控制台，所以它启动的所有东西（shell 命令、后台任务、挂载的 server、系统工具）
  都不带控制台窗口，用户桌面上不会突然弹出窗口。

## 4. 控制端常驻进程

控制端由两个程序组成：

- `ai-remote agent` 是每个用户一个的常驻进程。它持有控制端身份和配对信息，对每台被控端保持一条连接；
  每个 MCP 会话都是这条连接上的一条流。
- `ai-remote mcp` 是 MCP 客户端启动的程序。它连到常驻进程（需要时先启动它），把 stdio 转发过去，
  客户端关闭 stdin 后退出。

常驻进程按需启动：`pair`、`hosts`、`unpair`、`call`、`mcp` 发现它没在运行时，都会在后台把它拉起来
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

## 5. 配对步骤

1. **确认被控端在运行**（在被控端执行 `ai-remote host status` 或 `ai-remote status`）。
2. **在被控端生成票据：**

   ```sh
   ai-remote pair-code --level control --name laptop
   ```

   `--level` 可以是 `observe`、`control`（默认）或 `admin`：先给够用的最低级别。`--name` 是被控端给兑换这张票据的
   控制端起的名字（默认用控制端建议的名字，也就是它的机器名）。票据只能用一次，24 小时后过期。
3. **通过可信的渠道把票据交出去。** 票据就是凭证：谁先兑换，谁就拿到这个级别。
4. **在控制端兑换：**

   ```sh
   ai-remote pair <票据> --name office-pc
   ```

   `--name` 是你在本地给这台被控端起的名字（默认用被控端建议的名字）。命令会打印被控端的名称、
   由三个英文单词组成的安全码，以及授予的级别。
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
| `admin` | 再加 `shell`、`jobs_*`，以及挂载 server 中 admin 级的工具 |

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
| `the desktop is locked or showing a security prompt` | 锁屏、登录界面、UAC 提示 | 解锁；无人值守的机器关闭自动锁屏 |
| `runs in the services session (session 0)` | 被控端作为服务运行，或在 0 号会话中运行 | 改在用户会话里运行（`host install`） |
| `an ai-remote approval prompt is open on the host` | 有审批框在等回答，输入暂停 | 等机主回答后重试 |
| `the input would go to one of ai-remote's own windows` | 目标是审批框或被控端所在的终端 | 换个窗口操作 |
| `running with higher privileges` | 目标窗口属于以管理员身份运行的程序 | 以管理员权限运行被控端（由管理员执行 `host install`） |
| `is refused: it locks the workstation`（及类似提示） | 锁屏或注销的组合键 | 不要发送，交给机主决定 |
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
  ok    relay      connected to https://...; 3 direct address(es)
  ok    approval   approval prompts can be shown on this desktop
  ok    firewall   inbound connections allowed for C:\...\ai-remote-hostw.exe (UDP, TCP)
  ok    autostart  Task Scheduler task \ai-remote\host-1a2b3c4d starts the background host at login
  ok    audit      audit log writable: C:\...\host\audit.jsonl
  ok    disk       212.4 GiB free on C:\
Overall: warn
```

| 检查项 | 何时为 ok | 不是 ok 意味着 | 修复办法 |
|---|---|---|---|
| `session` | 处于交互式 Windows 会话（不是 0 号会话） | **fail**：服务会话没有桌面，截图全黑，输入没有去处 | 从用户会话中启动被控端；`host install` 就是这样注册的 |
| `console` | 会话连接在物理控制台上 | **warn**：这是远程桌面会话；断开后截屏和输入都会停止，控制台保持锁定 | 不要直接断开，而是把会话交还给控制台：在管理员命令行执行 `tscon <会话> /dest:console` |
| `desktop` | 输入桌面是 `Default`（未锁定） | **fail**：已锁定，或正显示登录界面、UAC 提示 | 解锁；无人值守的机器关闭自动锁屏 |
| `elevation` | 被控端以管理员权限运行 | **warn**：以管理员身份运行的程序的窗口会忽略它的输入 | 以最高权限运行（由管理员执行 `host install`） |
| `dpi` | 逐显示器 DPI 感知（v2） | **fail**：在有缩放的显示器上点击位置会偏 | 去掉可执行文件上的高 DPI 兼容性覆盖设置（属性 > 兼容性） |
| `relay` | 已连上主中继（home relay） | **warn**：没有使用中继，或一个都连不上；其他网络里的控制端可能连不进来 | 检查网络连接、出站防火墙或代理（中继走 HTTPS），或检查 `[network] relays` |
| `approval` | 能弹出审批框 | **warn**：需要审批的操作都会被拒绝 | 解锁桌面，或用 `[[rule]]` 放行可信的工具 |
| `firewall` | 有入站允许规则覆盖被控端程序的 UDP，或防火墙已关闭 | **fail**：有阻止规则作用于该程序（流量会退回中继）；**warn**：没有允许规则（Windows 会在第一次连接时弹提示），或者有针对所有程序入站 UDP 的阻止规则 | 在管理员终端执行 `host install`；其他程序的规则在 `wf.msc` 里检查 |
| `autostart` | 任务（或 LaunchAgent）会用本配置目录启动本次安装的后台被控端 | **warn**：没有安装，或者运行的是别的程序 | （再）执行一次 `ai-remote host install` |
| `audit` | 审计日志能以追加方式打开 | **fail**：操作不会被记录 | 修正配置目录的权限，或换一个 `--home` |
| `disk` | 配置目录所在磁盘剩余 5 GiB 以上 | 低于 5 GiB 为 **warn**；低于 0.5 GiB 为 **fail**（日志和审计日志可能写不进去） | 清理磁盘空间 |

防火墙检查看的是运行被控端的那个程序：有被控端在运行时，看它的可执行文件；没有时，看自启项将要运行的那个。

## 9. 审批

调用的动作为 `ask` 时需要审批。这是 `shell` 和 `jobs_start` 的默认动作，也是两类挂载工具的默认动作：
没有 `approve` 设置的 admin 级挂载工具，以及设置了 `approve = "ask"` 的挂载工具。此外，任何策略规则写了 `ask`
时同样需要审批（见 [tools.zh-CN.md](tools.zh-CN.md#8-策略规则)）。

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

被控端前的人在提示里确认这把新密钥。如果那边桌面不可用，就在设备上用 `ai-remote approver` 打印密钥，再到被控端的终端里
添加：`ai-remote approvers add <设备> <算法>:<密钥>`。之后：

```bash
ai-remote approvals                                  # 正在等待的审批及其 id
ai-remote approve 3f9c2a                             # 通过 Windows Hello 后，调用开始执行
ai-remote deny 3f9c2a --note "use the recycle bin"   # Agent 会收到这句话
```

没人坐着的机器可以设置 `approval_prompt = false`，并调长 `approval_timeout_secs`。设计、签名的消息和威胁模型见
[approvals.zh-CN.md](approvals.zh-CN.md)。

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

## 12. 自建中继

被控端默认使用 n0 的公共中继。设备借助中继找到彼此，在没有直连路径时也由中继转发流量；中继只能看到它解不开的
加密流量。要使用自己的 [iroh-relay](https://github.com/n0-computer/iroh) 服务器，把它们的 URL 写进被控端配置：

```toml
[network]
relays = ["https://relay1.example.org", "https://relay2.example.org"]
```

- 列表为空（或没有 `[network]` 段）表示使用默认的公共中继。
- 这项设置在被控端启动时读取。配对票据里记录着生成票据时被控端所用的中继，所以改完设置后要重新生成票据。
- 在被控端执行 `ai-remote doctor` 可以看到是否连上了中继（`relay` 检查）。
- M0 中控制端常驻进程没有中继设置，它自己的连接使用默认中继。

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
`direct`、`relay` 或 `unknown`。走中继也能用，只是延迟更高。被控端上的常见原因：有针对被控端程序的防火墙阻止规则
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
