# 本地浏览器客户端

> 英文原文：[WEB.md](../WEB.md)。
> 最后与英文同步日期（last synced with English revision）：2026-11-25。

`codewhale web` 通过统一的 Runtime API 打开 Codewhale 内嵌的浏览器客户端。
这是一个本地界面：服务器始终绑定到 `127.0.0.1`，不能改为局域网地址，
也不能在关闭 Runtime 身份验证的情况下运行。

## 启动

在希望 Codewhale 操作的工作区中运行：

```bash
codewhale web
```

默认地址为 `http://127.0.0.1:7878`。如果本地端口被占用，可以换一个回环地址上的端口：

```bash
codewhale web --port 8788
```

Codewhale 会启动 Runtime API，提供已内嵌在所安装二进制文件中、无需额外依赖的客户端，
打印一个只能使用一次的启动 URL，并请求操作系统在默认浏览器中打开它。
如果浏览器没有自动打开，请在十分钟内使用打印出的 URL。
按 `Ctrl+C` 停止进程，浏览器会话也会随之结束。

## 浏览器中的功能

内嵌客户端提供适应窗口大小的线程列表与搜索侧栏、由 Runtime 管理的会话信息、
对话记录、工具执行回执和输入框。你可以创建、选择、重命名和归档线程，
启动回合或向正在执行的回合追加指导，中断工作，处理审批请求，
以及回答 Runtime 发起的用户输入请求。

新线程默认使用配置中的 provider 和模型。创建线程时，命名自定义路由会保留其精确的
`[providers.<name>]` 身份，即使多个路由都属于 `custom` 类型，也能据此加载模型并创建线程。
为新线程选择路由不会更改 Runtime 默认值。Provider endpoint 和凭据在服务端配置；此界面只选择已配置的路由和模型。

浏览器是同一个本地 Runtime 的另一种视图。
它不会创建第二个云账户、把提供商（provider）的凭据复制到浏览器存储中，
也不会降低已配置的审批和沙箱策略要求。

## 提供共享应用构建

`codewhale web` 默认提供内嵌客户端。你也可以改为从磁盘目录提供共享网页应用
（`codewhale-app` 仓库中 `apps/web` 的构建产物）。这是可选功能；在共享构建达到功能对等之前，
内嵌客户端仍是默认选项。

1. 在 `codewhale-app` 中构建 `apps/web`，生成 `dist` 目录。
2. 将 `CODEWHALE_WEB_DIST` 指向该目录并启动命令：

   ```bash
   CODEWHALE_WEB_DIST=/path/to/codewhale-app/apps/web/dist codewhale web
   ```

该目录必须包含 `index.html`，且目录本身和 `index.html` 都不能是符号链接。
当变量指向有效目录时，启动时会打印一行信息，说明正在提供的目录。
当变量未设置或为空时，仍提供内嵌客户端。

已设置但无效的值不会被忽略：`codewhale web` 会拒绝启动并打印原因
（路径不是目录、是符号链接，或缺少 `index.html`）。请修正或取消设置该变量后重新运行命令。

如果已有一个 Codewhale 在运行（例如一个打开的终端会话），`codewhale web` 会附加到它上面，
由那个正在运行的 Codewhale 按它启动时的环境决定提供哪个客户端。如果你指定的目录不是它正在提供的目录，
命令会停止并说明原因，而不是打开另一个客户端：请退出正在运行的 Codewhale 后重新运行 `codewhale web`，
或取消设置该变量。当正在运行的 Codewhale 提供某个目录时，附加时也会打印同样的一行说明该目录。

该目录提供的内容：

- `/` 处的 `index.html`，以及所有无扩展名路径（例如 `/settings/plugins` 这类客户端路由）同样返回它。
- `/plugin-host.html`，应用用于嵌入插件界面的文档。
- `/static/*`，构建产生的带哈希的资源。

`/v1`、`/api` 和 `/__codewhale` 仍保留给 Runtime API 和引导凭证交换，该目录不会覆盖它们。
其他 `.html` 文件、未知文件类型以及非 `GET`/`HEAD` 请求返回 `404`。

上文的身份验证边界保持不变。`codewhale web` 需要 Runtime 身份验证，
Engine 会拒绝对 web 使用 `--insecure`。

## 身份验证边界

浏览器启动 URL 中包含一个随机、短时有效且只能使用一次的引导凭证（bootstrap capability），
其中始终不包含 Runtime bearer token。来自回环地址的请求会将引导凭证换成一个
带有 `HttpOnly`、`SameSite=Strict` 属性、仅对当前进程有效的会话 cookie，
随后引导凭证立即失效。

重复使用、已过期、格式错误或来自非回环地址的引导请求都会被拒绝。
Runtime token 不会出现在渲染后的 HTML、浏览器存储、URL 查询参数或片段，
以及浏览器启动参数中。一次性引导凭证会打印在本地终端中，
并短暂地经过操作系统的浏览器启动程序。虽然它只能使用一次且十分钟后过期，
但以同一操作系统用户身份运行的恶意进程仍处于本地信任边界之内。

使用 cookie 验证身份、且会改变状态的请求，还必须携带与本地网页完全匹配的来源（origin）。
跨源浏览器请求会被拒绝。现有显式使用 bearer token 或 Runtime token 请求头的客户端，
仍按原有 Runtime API 规则工作。

## 仅供本机使用

`codewhale web` 只接受 `--port`；此命令没有 `--host` 或关闭身份验证的选项。
不要将它当作公共网站，也不要通过路由器端口转发、公开反向代理或隧道直接暴露它的端口。

独立的 `codewhale app-server --mobile` 和 `--http` 模式有各自的部署与身份验证约定。
在运行任一模式之前，请先阅读 [RUNTIME_API.md](../RUNTIME_API.md)，
尤其是在选择非回环监听地址之前。

## 故障排查

- 如果端口 `7878` 已被占用，通过 `--port` 指定一个未使用的端口。
- 如果浏览器没有打开，请在十分钟内将打印出的一次性引导 URL 复制到同一台机器上的浏览器中。
  如果该 URL 已使用或已过期，请重新启动 `codewhale web`。
- 如果页面可以打开，但提供商不可用，请检查 `codewhale doctor` 和 `/provider`。
  web 命令不会配置或迁移提供商的凭据。
- 如果会话已过期，请停止并重新启动 `codewhale web`，以创建一个仅对新进程有效的会话。
  重复使用旧的引导 URL 应当失败。
- 通过手动输入地址打开的新标签页不会处于登录状态：会话的请求证明只保存在打开它的
  标签页中。请使用那个标签页、复制它，或重新启动 `codewhale web`。

有关集成端点、请求头、事件和完整的网页会话约定，请参阅 [RUNTIME_API.md](../RUNTIME_API.md)。
