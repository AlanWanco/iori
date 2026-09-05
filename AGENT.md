# iori Agent Guide

本文是 `iori` 仓库的协作和调试指南。它同时记录当前 HLS/直播链路审查的结论，方便后续代理在不破坏工作区的前提下继续工作。

如果本文与当前源代码、测试结果或用户最新要求冲突，以源代码、测试结果和用户要求为准。本文中的现场 URL、测试结果和问题状态可能随站点变化，不能替代重新验证。

## 1. 项目目标

`iori` 是一个支持 HLS 和 MPEG-DASH 的 VoD/live 下载器，核心职责包括：

- 解析 master/media playlist。
- 选择并拉取音频、视频及其他轨道。
- 并发下载、缓存、重试、排序和合并。
- 将下载结果写入文件、管道或 ffmpeg/RTMP 输出。
- 通过插件识别平台 URL 并取得真实播放地址。
- 为需要登录、Cookie、WebSocket 或专用 API 的平台提供适配层。

## 2. 仓库结构

| 路径 | 作用 |
|---|---|
| `bin/shiori` | 主 CLI，负责参数解析、插件注册、inspect 和 download 流程。 |
| `bin/minyami` | 较早的命令行入口，也直接使用核心 `StreamingSource`。 |
| `crates/iori` | 下载核心、缓存、并发下载、HLS/DASH source、merger、HTTP 工具。 |
| `crates/iori-hls` | 基于 `quick-m3u8` 的 HLS 模型和解析器。 |
| `crates/iori-ffmpeg` | 可选的 ffmpeg/native 集成；本机和 CI 可能需要额外的 FFmpeg 配置。 |
| `crates/ssa` | Sample-AES 等解密逻辑和相关夹具。 |
| `crates/uri-match` | URL 匹配辅助逻辑。 |
| `platforms/*` | 平台 API、页面、播放信息和专用 source。 |
| `plugins/plugin` | 插件接口、inspect 类型和插件管理器。 |
| `plugins/plugin-*` | 各平台在 `shiori` 中使用的 inspector/plugin 壳。 |
| `scripts` | 发布、构建和辅助脚本。 |
| `.versions/shiori` | 项目约定的构建/版本记录。 |

工作区由根目录 `Cargo.toml` 的 `bin/*`、`crates/*`、`plugins/*`、`platforms/*` 组成，edition 为 Rust 2024。

## 3. 核心数据流

### Inspect 阶段

大致流程如下：

```text
用户 URL
  -> bin/shiori 注册的 PluginManager
  -> Inspect::inspect()
  -> InspectPlaylist / Playlists
  -> playlist_url、playlist_type、headers、cookies、source metadata
```

`InspectPlaylist` 必须尽量给出正确的 `playlist_type`。如果平台已经确认返回 HLS，应设置 `PlaylistType::HLS`，不要依赖通用层猜测，否则日志会出现 `Unknown playlist type` fallback。

### Download 阶段

```text
InspectPlaylist
  -> DownloadCommand / ExtraOptions
  -> IoriContext
  -> StreamingSource::segments_stream()
  -> ParallelDownloader
  -> CacheSource + Merger
  -> 文件、stdout、ffmpeg 或 RTMP
```

`IoriHttp` 维护共享的 `CookieStoreMutex`。`IoriHttp::client()`、`builder()` 创建的带 Cookie Client，以及使用同一 `IoriHttp` clone 的后台任务共享这份 Cookie store；`raw_builder()`/`raw_client()` 则不自动挂载 Cookie provider，适合需要无状态请求的 API。

不要把 `InspectPlaylist` 中的 Cookie、session、Bearer、签名 m3u8 URL 写入普通日志、测试断言或提交记录。

## 4. HLS 核心行为

### Playlist source

- `HlsPlaylistSource::load_streams()` 先取得 playlist。
- 输入是 master playlist 时，按分辨率、帧率、带宽选择 variant，并尝试加载独立 audio/video alternative。
- 输入是 media playlist 时，创建单一 video stream。
- `HlsMediaPlaylistSource` 负责 media sequence、discontinuity、key、map、byterange、segment sequence 和 URL 解析。
- `HlsLiveSource` 在后台轮询 live media playlist，并向下游发送 segment batch。

### 初始切片限制

`HlsLiveSource::with_initial_segment_limit(Some(n))` 只限制第一次 playlist fetch：

- 首批只保留每条轨道最后 `n` 个 segment。
- 为避免 `OrderedStream` 等待原始高序号，截断后将首批 sequence 重新编号为从 `0` 开始。
- 同时重置 source 内部 Atomic sequence，使后续 segment 接在首批之后。
- 该选项降低长 VOD 缓冲伪直播接入 ffmpeg 的启动延迟，但不是总下载数量限制；对普通 VOD 不能用它截断整个归档。

### Live recovery

- manifest 拉取/解析失败使用 `MANIFEST_RECOVERY_DELAY` 重试。
- `idle_timeout` 可用于没有 `ENDLIST` 的伪直播归档。
- 非 manifest 的 map、key、URL 或 segment 处理错误不应被无限吞掉；当前实现会把错误发送给消费者并结束 source。
- playlist 的 stale/restart 判断使用 segment URL、byterange、media sequence 和 discontinuity part index。
- 如果 playlist 重启后复用 URI，媒体序号必须参与 identity，避免把新 stream 误判为旧窗口。

### 初始化片段和 byterange

`EXT-X-MAP` 初始化片段必须：

- 检查 HTTP 状态码，不能把 404/HTML 错误页当作 init bytes。
- 对 `BYTERANGE` 发送正确的 `Range: bytes=start-end`。
- 需要覆盖 map 失败和 map byterange 的 wiremock 测试。

### 音视频同步

`StreamingSegment::synchronization_key()` 默认返回 `None`。HLS segment 使用精确的：

```text
(part_index, media_sequence)
```

`ParallelDownloader` 的约定：

- key 相同的 audio/video segment 作为一个同步组下载。
- 同步组任意成员失败时，整个组都进入 `Merger::fail()`，避免产生音视频错位。
- key 不同的 segment 不得因为 stream type 相同、sequence 相近或 batch 相邻而合并。
- 没有 synchronization key 的自定义 segment 保持原有独立下载行为。

相关回归测试位于 `crates/iori/tests/downloader/parallel.rs`。

### PipeMerger

- `PipeMerger` 支持文件、stdout 和 ffmpeg mux 输出。
- `SegmentBuffer` 用于在 live relay 启动前积累 segment。
- live/有 buffer/自定义 ffmpeg 命令时，ffmpeg 输入使用 `-re -readrate_catchup 1.25`，避免 interleaved A/V pipe 长期落后。
- broken pipe 会尝试重启 ffmpeg；修改这部分时必须检查初始进程、重启进程、audio pipe、video pipe、stderr task 和 child wait task 的生命周期。
- `finish()` 会关闭 sender、等待 merger task，然后按 `recycle` 清理缓存。
- 直播输出 URL 中的 `&` 必须被 shell 引号包住。

## 5. 平台适配说明

### Niconico / NicoLive

相关文件：

- `platforms/nicolive/src/program.rs`
- `platforms/nicolive/src/watch.rs`
- `platforms/nicolive/src/source.rs`
- `platforms/nicolive/src/danmaku.rs`
- `plugins/plugin-niconico/src/lib.rs`

主要流程：

- `NicoEmbeddedData` 从 live 页面取得 program/channel 信息和 WebSocket URL。
- `WatchClient` 建立 WebSocket、发送 `startWatching`、处理 ping、seat、stream、message server 和 statistics。
- `stream` 消息提供 HLS URI 和 Cookie。
- plugin 返回 HLS playlist，并可选下载弹幕。
- `--nico-user-session`、`--nico-chase-play`、`--nico-download-danmaku`、`--nico-reserve-timeshift` 和 `--nico-danmaku-only` 由 CLI 透传。

当前重连修复：

- `WatchClient` 保存初始 Client，重连时复用它，避免丢失代理和 Cookie。
- `recv()` 返回 `Ok(None)` 时，调用方会记录断线并尝试重连，而不是继续空转。
- 仍需要本地 mock WebSocket 测试。
- 重连收到的新 stream URI/Cookie 是否能更新已经运行的 HLS source，尚未完成验证；不要把“WebSocket 重连成功”当作“播放链路已恢复”。

实时 NicoLive 测试使用具体直播 ID，容易因直播结束、页面变化或登录状态失效而失败。默认测试套件中这类测试应保持明确的 `#[ignore]`，并另外保留可重复的协议/mock 测试。

### Sheeta、nicochannel+ 和 qlover+

相关文件：

- `platforms/sheeta/src/client.rs`
- `platforms/sheeta/src/model.rs`
- `plugins/plugin-sheeta/src/lib.rs`

匹配 URL 形态：

```text
https://<host>/<channel>/(video|live)/<video_id>
https://<host>/(video|live)/<video_id>
```

当前实现注意事项：

- channel 是可选的命名 capture，video ID 不应包含 query 或 fragment。
- 有 channel 时先调用 `content_providers/channel_domain` 获取频道级 `fc_site_id`。
- session 请求和 video 请求带 `fc_site_id`、`fc_use_device: null`、Origin 和 Chrome User-Agent。
- API 请求必须调用 `error_for_status()`，这样 403/404 不会被错误解析成“缺少 data 字段”。
- 返回的 session HLS URL 应明确标记 `PlaylistType::HLS`。
- qlover/nicochannel 的站点 ID 是频道相关的，不能把根站点 ID 硬编码后复用到所有频道。

已知现场事实：

- `https://nicochannel.jp/kaorin/video/smbwkhi8sZApTqAF2sEtfqxm` 能成功 inspect，返回 HLS；此前完整归档也成功得到 H.264 1280x720 + AAC 44.1 kHz。
- qlover 的旧内容测试当前在 session endpoint 返回 403，需要有效登录会话或新鲜内容 ID；这不是仅靠正则可以修复的问题。

### Eplus

相关文件：

- `platforms/eplus/src/lib.rs`
- `platforms/eplus/src/model.rs`
- `platforms/eplus/src/source.rs`
- `plugins/plugin-eplus/src/lib.rs`
- `bin/shiori/src/commands/download.rs`
- `bin/shiori/src/commands/inspect.rs`

`EplusSource` 包装 `HlsLiveSource`，并使用共享 Cookie store：

- inspect 阶段获取登录 session、CloudFront Cookie、playlist 和 event URL。
- download 阶段将需要的 Cookie 注入 playlist 域和 event/status 域。
- 后台任务默认每 30 分钟刷新 CloudFront Cookie。
- status API 使用无状态 Client，避免旧 Cookie 污染刷新请求。
- status API 没有返回 CloudFront Cookie 时，保留现有 CloudFront Cookie，不应无条件清空整个 Cookie jar。
- status probe 失败时才回退到 event page；event page 没有新 CloudFront Cookie 时也必须保留旧值。

已知风险：后台 refresh task 当前是无限 loop，未来需要用 cancellation token 或明确的 JoinHandle 生命周期收尾。

### Vimeo

仓库没有专用 Vimeo plugin。Vimeo event embed 解析出的新鲜签名 m3u8 走通用 HLS inspector/source。

- archive 级别已实测成功。
- 签名 URL 可能过期，archive 成功不代表长期 live refresh 已验证。
- 不要把完整带签名的 CDN URL 写进 `AGENT.md`、源码、日志或提交信息。

## 6. 开发和验证流程

### 修改前

先执行并阅读：

```bash
git status --short
git diff --stat
git diff
git log --oneline -10
```

工作区可能已有用户或其他代理的修改：

- 不使用 `git reset --hard`、`git checkout --` 或其他破坏性回滚命令。
- 不覆盖不相关文件的未提交修改。
- 如果同一个文件已有修改，先阅读 diff，再将新改动叠加到现有内容上。
- 只提交用户明确要求的文件；默认不 commit、不 push。

### 常用命令

```bash
cargo build -p shiori
cargo test -p iori
cargo test -p iori-hls
cargo test -p iori-sheeta
cargo test -p iori-nicolive
cargo test -p iori-eplus
cargo test -p shiori-plugin-sheeta
cargo test -p shiori-plugin-niconico
cargo clippy -p iori -p iori-sheeta -p iori-nicolive -p shiori-plugin-sheeta -p shiori-plugin-niconico --all-targets
git diff --check
```

在不运行已知问题夹具时，可以使用：

```bash
cargo test --workspace --exclude iori-ffmpeg --exclude iori-ssa --quiet
```

完整 `cargo test --workspace` 可能受 `iori-ffmpeg` 的本机 FFmpeg 链接配置影响。当前仅排除 `iori-ffmpeg` 时，`crates/ssa/tests/decrypt.rs::decrypt_ac3` 仍可能失败；这属于 SSA 解密夹具问题，不能误报为 HLS 改动回归。

`cargo fmt --all -- --check` 目前会报告部分未触及文件的既有格式差异。不要为了清理全仓格式而覆盖用户改动；可对本次改动文件单独运行 rustfmt，并保留全量格式检查的限制记录。

### 真实流验证

优先使用 inspect，避免不必要地下载长归档：

```bash
target/debug/shiori inspect 'https://nicochannel.jp/<channel>/video/<content_id>'
target/debug/shiori inspect 'https://qlover.jp/<channel>/video/<content_id>'
```

下载验证时：

- 使用临时目录和短窗口；`--initial-segments` 只降低 live 启动延迟，不会截断普通 VOD。
- 需要确认错误码时，保存日志但先检查日志是否含 Cookie、session、Bearer 或签名 URL。
- 使用 `ffprobe` 验证最终文件是否有 video/audio stream，并使用 `ffmpeg -v error` 检查解复用错误。
- 完成测试后检查是否有残留 `shiori`、`ffmpeg`、ffmpeg relay 或后台代理进程。
- RTMP 输出必须整体引用，例如：

```bash
target/debug/shiori download --no-tui --no-recycle --initial-segments 5 \
  --output 'rtmp://<host>/<app>/<stream>?key=<redacted>&schedule=<redacted>' \
  'https://<platform>/<path>'
```

不要在命令、测试输出或文档中填写真实推流 key。

## 7. 当前验证基线

截至本指南更新时，已完成以下验证：

- `cargo test -p iori`：53 个 unit test、30 个主集成测试、source/streaming 测试全部通过。
- `cargo test -p iori-sheeta`：2 个确定性测试通过，1 个需要 live API session 的测试 ignored。
- `cargo test -p iori-nicolive`：2 个确定性测试通过，2 个需要实时 NicoLive 的测试 ignored。
- `cargo test -p iori-eplus`：Cookie refresh 回归测试通过。
- `cargo build -p shiori`：通过。
- `cargo test --workspace --exclude iori-ffmpeg --exclude iori-ssa --quiet`：通过。
- clippy 已无本次修改引入的核心警告；剩余主要是 `InspectResult` 大枚举和生成 protobuf 大枚举。
- `git diff --check`：通过。
- 最后检查没有残留 `shiori`/`ffmpeg` 进程。

已完成的现场验证：

- Vimeo `https://vimeo.com/event/6122564/embed`：新鲜签名 HLS archive 成功，约 15 秒，H.264 1920x1080 + AAC 48 kHz。
- nicochannel `https://nicochannel.jp/kaorin/video/smbwkhi8sZApTqAF2sEtfqxm`：inspect 当前返回 `Playlist Type: HLS`；此前完整归档约 4448 秒且音视频轨道有效。
- Eplus 本地 relay：此前实测 A/V 管道约 `1.01x`，ffmpeg 能持续输出。

## 8. 当前未完成事项

- 用有效 qlover 登录会话或新鲜内容 ID完成端到端 session/HLS 验证。
- 用新鲜 NicoLive session 验证 WebSocket 断线、重连、Cookie 和 playlist 更新。
- 为 `WatchClient` 重连增加本地可重复 mock WebSocket 测试。
- 评估 Eplus refresh task 的取消和 child task 生命周期。
- 轮换外部脚本中的硬编码凭据，并将认证信息迁移到环境变量或本地未跟踪配置。
- 决定是否单独处理既有全仓 rustfmt 差异和 `iori-ssa` 的 `decrypt_ac3` 夹具失败。

## 9. 安全规则

安全审查发现以下外部脚本包含硬编码认证信息或敏感请求数据：

- `/Users/alanwanco/Workspace/code-repository/Radio/kito_fukumimi_polling.py:43-47`
- `/Users/alanwanco/Workspace/code-repository/Radio/vimeo_api_test.py:15-27`
- `/Users/alanwanco/Workspace/code-repository/Radio/sashidega_scrape_metadata.py:14-15`
- `/Users/alanwanco/Workspace/code-repository/Radio/batch_download.py:15-22,44+`

处理要求：

- 不复制真实值到源码、Markdown、commit message 或聊天回复。
- 认为已经出现在 shell history、临时日志或外部脚本中的 token 可能已泄露，优先撤销和轮换。
- 测试命令使用 `<redacted>` 占位符或通过环境变量注入。
- 除非用户明确要求，不修改 `/Users/alanwanco/Workspace/code-repository` 下的外部脚本。
- 不因“测试方便”而打印完整 Cookie jar、签名 URL 或推流地址。

## 10. 版本记录约定

项目沿用 `.versions/shiori` 的记录格式：

```text
shiori-v0.3.0-awcfork-<真实 git commit hash>
```

当前最近提交为 `bd4c826`，对应已有版本记录。完成代码修改并形成真实 commit 后，按仓库约定追加真实 hash；不要为未提交工作区伪造 hash。仅修改本文档时不需要新增版本号。

## 11. 完成任务前清单

- 阅读并保留已有 `git status` 和 diff。
- 为核心逻辑增加或更新可重复测试。
- 对需要外部站点的测试标明依赖和失败原因，不把 403、过期内容或缺少依赖误判为代码回归。
- 运行相关 crate 测试、构建和 `git diff --check`。
- 检查日志、shell history、临时文件和命令行参数是否包含 secret。
- 检查没有残留下载器、ffmpeg 或 relay 进程。
- 最终报告列出已验证事实、阻塞项、未提交文件和下一步，不声称未执行的测试已通过。
