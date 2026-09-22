# iori

A brand new HLS / MPEG-Dash stream downloader, with support for both VoD and Live streaming.

## Download

You can get the pre-compiled executable files from `artifacts`. (Like [this](https://github.com/Yesterday17/iori/actions/runs/11423831843))

## Project Structure

- `bin`: Contains the main executable crates, like `minyami` and `shiori`.
- `crates`: Core library crates, such as `iori` (the downloader core) and `iori-ssa` (Sample-AES decryption).
- `plugins`: Plugin system related crates, like `shiori-plugin`, `shiori-plugin-showroom`.
- `platforms`: Video platform-specific implementations, such as `iori-nicolive` and `iori-showroom`.

## Quickstart

### 导入推流地址
```
#微博推流
export WEIBO_RTMP="rtmp://pswb.live.weibo.com/alicdn/"
export WEIBO_RTMP_ID=""
export WEIBO_RTMP_KEY="?auth_key=-0-0-"

echo "$WEIBO_RTMP"
echo "$WEIBO_RTMP_ID"
echo "$WEIBO_RTMP_KEY"
echo "m3u8地址：https://plwb.live.weibo.com/alicdn/${WEIBO_RTMP_ID}.m3u8"

#b站推流
export BILI_RTMP="rtmp://live-push.bilivideo.com/live-bvc/"
export BILI_RTMP_KEY="?streamname=live_&key=&schedule=rtmp&pflag=2"

echo "$BILI_RTMP"
echo "$BILI_RTMP_KEY"
```
### eplus推流
```
# 日区限定
export EPLUS_USER="youremail@gmail.com" 
export EPLUS_PWD="password" 
# 日区/非日区共用，不支持dash
export EPLUS_URL="https://live.eplus.jp/<event_id>"

echo "$EPLUS_USER"
echo "$EPLUS_PWD"
echo "$EPLUS_URL"

# 保留所有分片+只加载最新三条地址
shiori-dev download "${EPLUS_URL}" \
--eplus-username "${EPLUS_USER}" \
--eplus-password "${EPLUS_PWD}" \
--pipe-mux --initial-segments 3 \
--output "${WEIBO_RTMP}${WEIBO_RTMP_ID}${WEIBO_RTMP_KEY}" \
--no-tui --no-recycle --no-live-idle-timeout --wait
```
### nico转播
```
export NICO_LIVE="https://live.nicovideo.jp/watch/lv34567890"
export NICO_SESSION=""

echo "$NICO_LIVE"
echo "$NICO_SESSION"

# 保留所有分片+只加载最新三条地址
shiori-dev download "$NICO_LIVE" \
--no-tui --nico-user-session "$NICO_SESSION" \
-M -o "${BILI_RTMP}${BILI_RTMP_KEY}" --initial-segments 3 --no-recycle
```
### Sheeta
```
export RTMP_URL=""
export SHEETA_URL=""
# 支持
# https://nicochannel.jp/miitanu2525/live/sm7BFk88kF85uNUsLnxjjhtW
# https://qlover.jp/hayaraki/live/smcpDr6u9LnNBNjL74DckHDo
# https://audee-membership.jp/okuma-wakana/live/smUgMcfCHJFoF9fvA4M6czgW

# 保留所有分片+只加载最新三条地址
shiori-dev download "${SHEETA_URL}" -M --output "${RTMP_URL}" --no-tui --wait --initial-segments 3 --no-recycle
```
### m3u8普适性转播
```
export SOURCE_M3U8=""
export SOURCE_HLS_KEY="" # 可选
echo "$SOURCE_M3U8"
echo "$SOURCE_HLS_KEY"

# 保留所有分片+只加载最新三条地址
shiori-dev download "${SOURCE_M3U8}" \
--key "${SOURCE_HLS_KEY}" --pipe-mux --initial-segments 3 \
--output "${WEIBO_RTMP}${WEIBO_RTMP_ID}${WEIBO_RTMP_KEY}" \
--no-tui --no-recycle
```


## Road to 1.0

- [ ] Separate decrypt and download
- [ ] Support `EXT-X-DISCONTINUITY` for HLS
- [ ] Support custom descryption logic
- [ ] Support custom `StreamingSource` for plugins
