# Eplus 插件开发笔记

## 一、改了什么

### 新增文件（2 个 crate）

| 路径 | 作用 |
|---|---|
| `platforms/eplus/` | eplus.jp 平台 API 客户端（登录、页面解析、流选择） |
| `plugins/plugin-eplus/` | shiori 插件壳，负责 URL 匹配和 inspect 流程 |

### 修改的已有文件

| 文件 | 改动 |
|---|---|
| `Cargo.toml`（根） | 加两个 workspace dependency 声明 |
| `bin/shiori/Cargo.toml` | 加 `iori-eplus` + `shiori-plugin-eplus` 依赖 |
| `bin/shiori/src/commands/inspect.rs` | 注册 `EplusPlugin`（2 行） |
| `bin/shiori/src/commands/download.rs` | **主要改动**：`ExtraOptions` 加 `platform`/`original_url` 字段；`From<InspectPlaylist>` 传递这两个字段；HLS 分支加 eplus 特殊路径用 `EplusSource`；`download()` 入口加 cookie 双域名注入 |
| `crates/iori/src/util/http.rs` | 加 `export_cookies_for_url()` 方法 |

## 二、如何使用

```bash
# 直播（需要登录）
shiori download \
  --eplus-username 'your@email.com' \
  --eplus-password 'yourpass' \
  -o output.ts \
  'https://live.eplus.jp/ex/player?ib=XXXX'

# 或者直接事件页面 URL
shiori download \
  --eplus-username 'your@email.com' \
  --eplus-password 'yourpass' \
  -o output.ts \
  'https://live.eplus.jp/SOME-EVENT-ID'

# 偏好 VOD/回放而非直播流
shiori download \
  --eplus-username 'your@email.com' \
  --eplus-password 'yourpass' \
  --eplus-prefer-archive \
  -o output.ts \
  'https://live.eplus.jp/ex/player?ib=XXXX'

# 也支持 --initial-segments 减少启动延迟
shiori download \
  --eplus-username '...' --eplus-password '...' \
  --initial-segments 3 \
  -P \
  'https://live.eplus.jp/ex/player?ib=XXXX' \
  | ffmpeg -i pipe:0 -c copy output.mp4
```

不提供用户名密码也可以尝试匿名访问（部分免费活动可能不需要登录）。

### 转播功能目前可用的命令

```bash
# 方式 1: -P
# 纯 pipe 模式，shiori 把流输出到 stdout，交给外部 ffmpeg 推流/转封装
shiori download \
  --eplus-username '...' --eplus-password '...' \
  --initial-segments 3 \
  -P \
  'https://live.eplus.jp/ex/player?ib=XXXX' \
  | ffmpeg -re -i pipe:0 -c copy -f flv 'rtmp://127.0.0.1/live/test'

# 方式 2: -M
# shiori 内部启动 ffmpeg；有音频流时会自动做音视频 mux 后输出到目标地址
shiori download \
  --eplus-username '...' --eplus-password '...' \
  --initial-segments 3 \
  -M \
  -o 'rtmp://127.0.0.1/live/test' \
  'https://live.eplus.jp/ex/player?ib=XXXX'
```

目前建议：

- `-P` 适合你想自己完全控制 ffmpeg 参数的时候
- `-M` 适合直接转推，尤其是有独立音轨的流

## 三、具体逻辑

整个流程分两个阶段：

### 阶段 1：Inspect（插件识别 + 提取信息）

```
用户输入 URL
  → PluginManager 用正则匹配到 EplusInspector
  → EplusInspector.inspect()：
    1. 用 context.http.builder() 构建 reqwest::Client（共享 cookie store）
    2. 如果有用户名密码 → EplusClient::login()：
       a. GET 事件页 → 被 302 到登录页 → 拿 X-CLTFT-Token
       b. POST /member/api/v1/FTAuth/idpw（JSON body）
       c. POST 登录表单（form-encoded）
       d. session cookies 自动存入共享 cookie jar
    3. EplusClient::get_event_data(url)：
       a. GET 事件页（此时带 session cookie）
       b. 服务器返回 Set-Cookie: CloudFront-Policy=..., CloudFront-Signature=...
       c. reqwest 自动把 CloudFront cookies 存入 cookie jar
       d. 解析 HTML 中的 var app = {...}、var listChannels = [...]、var streamSession = '...'
    4. select_best_playlist()：分类 live vs vod URL，按偏好选择
    5. export_cookies_for_url() 导出两个域名的 cookies（name=value 格式）
    6. 返回 InspectPlaylist { playlist_url, cookies, source: { platform: "eplus", original_url } }
```

### 阶段 2：Download（下载 + Cookie 刷新）

```
InspectPlaylist → From → DownloadCommand
  → download()：
    1. 检测 platform == "eplus" → 提前 clone cookies
    2. http = self.http.into_client(&playlist_url)  // cookies 加到 stream.live.eplus.jp
    3. http.add_cookies(cookies, event_url)          // 同一批 cookies 也加到 live.eplus.jp ← cookie 双域名注入
    4. context = IoriContext { client: http.client() }  // client 共享同一个 Arc<CookieStoreMutex>
    5. 创建 EplusSource::new(http, playlist_url, event_url, key)
    6. EplusSource::segments_stream() 被调用时：
       a. 克隆 http → refresh_http（新 OnceLock，但共享同一个 Arc<CookieStoreMutex>）
       b. tokio::spawn 后台任务：每 45 分钟 GET event_url
          → reqwest 带 session cookies（来自 live.eplus.jp 域名）
          → 服务器返回新的 CloudFront Set-Cookie
          → reqwest 自动更新 cookie jar
       c. 委托给 inner HlsLiveSource.segments_stream(context)
          → 后续 segment 请求用 context.client → 共享 cookie jar → 自动带最新 CloudFront cookies
```

### Cookie 共享的关键链条

```
IoriHttp
  └── Arc<CookieStoreMutex>  (一份，所有人共享)
       ├── http.client()  → Client A（context.client，segment 请求用）
       ├── refresh_http.client() → Client B（刷新任务用）
       └── 两个 Client 都通过 cookie_provider() 共享同一个 jar
```

## 四、可能的 Bug 和调试方法

### Bug 1：登录流程可能和 Python 脚本有差异

**风险**：Python 脚本的登录流程是通过逆向得来的，eplus 随时可能改接口。`X-CLTFT-Token` 的获取方式（从 response header）不一定永远可靠——Python 脚本里是从 header 拿的，但也可能从 HTML meta tag 来。

**调试**：
```bash
RUST_LOG=debug shiori download --eplus-username '...' --eplus-password '...' 'URL'
```
看 `Step 1: Sending pre-login API request...` 和 `Step 2: Submitting login form...` 的日志。如果 `X-CLTFT-Token` 拿不到会直接报错。如果登录后被重定向回登录页也会报错。

### Bug 2：HTML 解析正则可能匹配失败（`.` 不匹配换行）

**风险**：`var app = {...}` 的正则 `<script>\s*var\s+app\s*=\s*(?P<data>\{.+?\});\s*</script>` 用了 `.+?`（非贪婪），但如果 JSON 跨行则不会匹配（`.` 不匹配换行）。Python 脚本用了 `re.DOTALL`，这里没有。

**位置**：`platforms/eplus/src/lib.rs:164`

**修复建议**：用 `(?s)` flag 或者 `[\s\S]+?` 代替 `.+?`：
```rust
Regex::new(r"(?s)<script>\s*var\s+app\s*=\s*(?P<data>\{.+?\});\s*</script>")
```

**调试**：用 `RUST_LOG=debug` 跑，如果出 `Could not find 'var app = {...}' in page` 说明正则没匹配上。可以加临时日志把 body 前 2000 字符打出来看。

### Bug 3：`var listChannels` 正则同样的跨行问题

**位置**：`platforms/eplus/src/lib.rs:210`

同样 `[\s\S]+?` 问题。如果 listChannels 的值跨多行就匹配不到。

### Bug 4：Cookie 刷新可能拿不到新 CloudFront cookies

**风险**：刷新任务只是 `GET event_url`。如果 eplus 服务器只在特定条件下（比如带特定 query param、或者需要 POST）才返回新的 CloudFront Set-Cookie headers，纯 GET 可能拿不到。

**调试**：看日志 `[eplus] CloudFront cookies refreshed successfully.` 出现后，下一轮 segment 请求是否还是 403。如果 403，说明刷新没真正更新 cookies。可以在刷新后加日志打印 cookie jar 内容：
```rust
// 调试用：刷新后打印 cookies
let cookies = refresh_http.export_cookies_for_url(&event_url);
log::debug!("[eplus] Cookies after refresh: {:?}", cookies);
```

### Bug 5：刷新任务永远不会停止

**风险**：`tokio::spawn` 的后台任务是一个无限 `loop`。当下载结束（`ParallelDownloader` drop）后，这个 task 还在跑。虽然不会造成功能问题（进程退出时会自动 kill），但如果未来有复用场景（比如一次下载多个流），旧的刷新任务会泄漏。

**位置**：`platforms/eplus/src/source.rs:74-101`

**改进方向**：用 `tokio_util::sync::CancellationToken` 或 `JoinHandle` + `AbortHandle`。

### Bug 6：`EplusAppData` 的 serde 字段名可能不匹配

**风险**：`EplusAppData` 没有 `#[serde(rename_all = "camelCase")]`，所以期望 JSON 字段名是 `app_id`、`app_name` 等 snake_case。但实际 eplus 的 JS 变量可能是 `appId`、`appName` 等 camelCase。

**位置**：`platforms/eplus/src/model.rs:4-17`

**调试**：如果 `get_event_data` 返回 `Failed to parse app data JSON`，大概率是字段名不匹配。用 `RUST_LOG=debug` 看解析错误的具体信息，或者临时把 regex match 的内容打出来和 struct 对比。

**修复**：需要看实际 eplus 页面的 `var app = {...}` 的 key 格式，如果是 camelCase 就加 `#[serde(rename_all = "camelCase")]` 或逐个 `#[serde(alias = "appId")]`。

### Bug 7：第二个 URL 正则可能误匹配

**位置**：`plugins/plugin-eplus/src/lib.rs:45`

```rust
Regex::new(r"https://live\.eplus\.jp/(?P<path>[^/]+)$")
```

这个正则会匹配 `https://live.eplus.jp/member`、`https://live.eplus.jp/login` 等非事件页面。如果用户不小心传了这种 URL，会尝试解析非事件页面然后报错。

## 五、对原有功能的影响

### 无影响的部分

- **`IoriHttp` 加了 `export_cookies_for_url()`**：纯新增方法，不改任何现有行为
- **`inspect.rs` 加了 `.add(EplusPlugin)`**：Plugin 按 URL 正则匹配，只有 `live.eplus.jp` 的 URL 才会触发，不影响其他插件
- **`Cargo.toml` / `Cargo.lock` 加新依赖**：只增不改

### 有潜在影响的部分

**`download.rs` 是唯一有风险的改动点：**

1. **`ExtraOptions` 加了 `platform` 和 `original_url` 字段**：都是 `Option<String>`，Default 为 `None`。非 eplus 流程这两个字段永远是 `None`。`ExtraOptions` 有 `#[derive(Default)]`，所以旧代码中所有 `..Default::default()` 的地方自动兼容。**无影响。**

2. **`From<InspectPlaylist>` 改动**：加了两行读 `data.source` 的代码。`data.source` 对非 eplus 插件可能是 `None`（`.as_ref().map(...)` 返回 `None`），也可能有值但 platform 不是 "eplus"。**无影响。**

3. **HLS 分支从直接创建 `HlsLiveSource` 变成了 `if is_eplus { ... } else { 原来的代码 }`**：`else` 分支和原来完全一样。只要 `platform != Some("eplus")`，走的就是 `else`。**无影响。**

4. **`download()` 入口的 cookie 双域名注入**：非 eplus 时 `eplus_event_cookies = None`，后面的 `if let Some(...)` 不执行。**无影响。**

5. **编译体积增加**：新增了 `iori-eplus` 和 `shiori-plugin-eplus` 两个 crate，shiori 二进制体积会略增。

**结论：所有改动都通过 `platform == "eplus"` 的条件守卫隔离，非 eplus 流程走的代码路径和改动前完全一致。**

## 六、关键文件索引

| 文件 | 说明 |
|---|---|
| `platforms/eplus/src/lib.rs` | API 客户端：登录、页面解析、流选择 |
| `platforms/eplus/src/model.rs` | 数据结构：EplusAppData、DeliveryStatus、EplusEventData、FtAuthResponse |
| `platforms/eplus/src/source.rs` | EplusSource：包装 HlsLiveSource + 后台 cookie 刷新 |
| `plugins/plugin-eplus/src/lib.rs` | 插件注册 + Inspector 实现 |
| `bin/shiori/src/commands/download.rs` | 下载入口，eplus 分支 + cookie 双域名注入 |
| `bin/shiori/src/commands/inspect.rs` | 插件注册点 |
| `crates/iori/src/util/http.rs` | IoriHttp，cookie store 共享机制 |
| 原始 Python 脚本 | `/Users/alanwanco/Workspace/code-repository/Eplus/eplus_download_all_auto.py` |
