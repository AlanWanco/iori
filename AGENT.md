# 任务总结: HLS 直播启动延迟优化 (--initial-segments)

## 任务目标
为 `iori` 下载器添加 `--initial-segments` 选项，用于在 HlsLive 模式下限制首次拉取的切片数量，从而减少将长 VOD 缓冲的直播流导入 ffmpeg 时的启动延迟。

## 已完成工作

### 1. 核心逻辑 (crates/iori)
- **`HlsLiveSource`**: 
    - 添加了 `initial_segment_limit: Option<usize>` 字段和 builder 方法。
    - 在 `segments_stream` 异步任务中实现了首次拉取的切片截断逻辑。
- **`HlsPlaylistSource`**:
    - 添加了 `reset_stream_sequences` 方法，用于在截断后重置内部 Atomic 计数器，确保后续切片编号连续。
- **Bug 修复**: 
    - 解决了截断后切片 sequence 编号过大导致 `OrderedStream` 内部阻塞的 Bug。现在截断后会重新编号 (0..N) 并同步重置 source 计数器。

### 2. 命令行界面 (bin/)
- **`shiori`**: 
    - 添加了 `--initial-segments` 选项。
    - 完整支持 i18n（中英文 `.ftl` 翻译文件），使用 `about_ll` 宏进行国际化。
- **`minyami`**: 
    - 同步添加了 `--initial-segments` 选项并透传至核心逻辑。

### 3. 平台支持 (platforms/)
- **`NicoTimeshiftSource`**: 实现了 `with_initial_segment_limit` 的透传，使该功能在 NicoNico 时移回放中同样可用。

## 验证结果
- **编译**: `cargo check` 与 `cargo build --release` (macOS arm64) 全部通过。
- **功能测试**: 
    - 测试 URL: `theater-complex.town` 某直播流（含有 ~2000 个分片的 VOD 缓冲）。
    - 结果: 使用 `--initial-segments 5` 后，成功将首批分片限制为最后 5 个，ffmpeg 立即识别到流并开始推流，延迟从数分钟降低至秒级。
- **CI**: 代码已 push 至 `AlanWanco/iori`，GitHub Actions 正在构建全平台二进制。

## 推荐用法 (FFmpeg 配合)
建议去掉 `-re` 以防二次延迟，并增加格式指定：
```bash
shiori download "URL" --key "KEY" --initial-segments 5 -P | \
ffmpeg -f mpegts -i pipe:0 -c copy -flvflags no_duration_filesize -f flv "rtmp://..."
```

## 修改文件列表
- `crates/iori/src/hls/live.rs`
- `crates/iori/src/hls/source.rs`
- `bin/shiori/src/commands/download.rs`
- `bin/shiori/i18n/en-US/shiori.ftl`
- `bin/shiori/i18n/zh-CN/shiori.ftl`
- `bin/minyami/src/main.rs`
- `platforms/nicolive/src/source.rs`
