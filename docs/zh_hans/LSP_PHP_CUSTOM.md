# LSP：PHP 内置支持与自定义语言服务器扩展

> v0.8.65+ | `codex/lsp-php-custom-servers`
>
> 英文原文：[LSP_PHP_CUSTOM.md](../LSP_PHP_CUSTOM.md)。
> 最后与英文同步日期（last synced with English revision）：2026-09-29。

## 概述

本次改动将 **PHP** 加入内置 LSP 语言注册表，并新增 `[lsp.custom]` 配置段，
允许用户按文件扩展名注册任意 LSP 服务器——覆盖内置 `Language` 枚举未包含的
语言（Ruby、C#、Swift、Lua 等）。

## 改动内容

### 1. PHP 内置支持

- 在 `crates/lsp/src/registry.rs` 的 `Language` 枚举中添加 `Php` 变体
- `.php` 文件自动检测，默认路由至 `intelephense --stdio`
- 用户可通过 `[lsp.servers].php` 覆盖默认命令

### 2. 自定义 LSP 服务器扩展

新增 `CustomLspDef` 结构体（定义于 `crates/lsp/src/lib.rs`，同步至
`crates/config/src/lib.rs`）：

```rust
pub struct CustomLspDef {
    pub language_id: String,  // textDocument/didOpen 使用的 LSP languageId
    pub command: String,      // 要启动的可执行文件
    pub args: Vec<String>,    // 参数（默认为空）
}
```

新增配置字段 `LspConfig.custom: HashMap<String, CustomLspDef>` —— 以文件扩展名
（不含前导点）为键，如 `"rb"`、`"cs"`、`"swift"`。

`LspManager::diagnostics_for` 中，当内置注册表返回 `Language::Other` 时，管理器
会先检查用户自定义表再放弃。自定义服务器拥有独立的按需（惰性）启动的 transport 映射和
每个扩展名仅一次的缺失告警（避免日志刷屏）。

### 3. Transport 通用化

`StdioLspTransport::spawn` 现接受 `&str language_id` 而非 `Language`，内置和
自定义服务器共享同一传输实现。`client.rs` 中移除了旧的 `Language` 导入。

### 4. 提取共享轮询管线

`poll_diagnostics` 是新提取的私有方法，内置和自定义诊断路径共用，消除了
重复的调用/等待/过滤/排序/截断逻辑。

## 配置

### 内置 PHP（若 `intelephense` 在 PATH 中则默认启用）

```toml
# 无需配置 —— .php 文件自动检测。
# 如需覆盖服务器：
[lsp.servers]
php = ["phpactor", "language-server"]
```

### 自定义语言服务器

```toml
[lsp.custom.rb]
command = "ruby-lsp"
args = ["--stdio"]
language_id = "ruby"

[lsp.custom.cs]
command = "csharp-ls"
language_id = "csharp"

[lsp.custom.swift]
command = "sourcekit-lsp"
language_id = "swift"
```

键为文件扩展名（不含前导点）。`args` 字段默认为空。
`language_id` 须与 LSP 服务器在 `textDocument/didOpen` 中期望的值匹配。

## 架构

```
edit_file / write_file / apply_patch 成功
        │
        ▼
  LspManager.diagnostics_for(file)
        │
        ├── custom_for_extension(file) ── 命中？──► transport_for_custom(ext, def)
        │                                                  │
        ├── detect_language(file) ── Other？──► 返回 None（跳过）
        │
        └── transport_for(lang)
                │
                ▼
          poll_diagnostics(file, text, transport)
                │
                ▼
          DiagnosticBlock → 注入会话的消息流
```

## 验证

```
cargo test -p codewhale-tui --lib lsp::
# 32 个测试通过（新增 3 个：detects_php_extension、language_ids_for_php、
# server_for_php_is_intelephense）
cargo clippy -p codewhale-tui --lib
# lsp 模块：零新增警告
```

## 涉及文件

| 文件 | 改动 |
|------|------|
| `crates/lsp/src/registry.rs` | +Php 变体、检测、服务器映射、测试 |
| `crates/lsp/src/lib.rs` | +CustomLspDef、LspConfig.custom、LspManager 自定义回退 |
| `crates/lsp/src/client.rs` | spawn 接受 &str language_id |
| `crates/tui/src/config.rs` | LspConfigToml.custom + into_runtime |
| `crates/config/src/lib.rs` | LspConfigToml.custom + CustomLspDef |
| `config.example.toml` | PHP 文档 + 自定义扩展示例 |
