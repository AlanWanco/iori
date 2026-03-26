shiori-about = Yet another m3u8 downloader

download-wait = Wait for stream to start when no stream is detected
download-no-tui = Disable TUI
download-url = URL to download

download-http-headers = Additional HTTP headers for all HTTP requests, format is key: value
download-http-cookies =
    {"["}Advanced] Additional HTTP cookies

    Will not take effect if `Cookies` is set in `headers`.
    Do not use this option unless you know what you are doing.
download-http-timeout = HTTP timeout, in seconds
download-http-http1-only = Force to use HTTP/1.1 for requests
download-http-proxy = Use the specified HTTP/HTTPS/SOCKS5 proxy

download-concurrency = Threads limit
download-segment-retries = Segment retry limit
# download-segment-retry-delay = Set retry delay after download fails in seconds
download-manifest-retries = Manifest retry limit
download-initial-segments =
    Only keep the last N segments from the initial playlist fetch.

    Reduces startup latency for live streams with a long VOD buffer
    (e.g. when piping to ffmpeg for restreaming).
    If the playlist has fewer segments than N, all segments are kept.

download-cache-in-menory-cache = Use in-memory cache and do not write cache to disk while downloading
download-cache-temp-dir =
  Temporary directory

  The default temp dir is the current directory or the system temp dir.
  Will not take effect if `cache_dir` is set.
download-cache-cache-dir =
    {"["}Advanced] Cache directory

    Speficy a directory to store cache files.

    If specified, the cache will be stored in this directory directly without creating a subdirectory.
download-cache-experimental-stream-dir-cache =
  {"["}Experimental] Use new cache directory structure
  
  Resume download is supported in this cache source. Make sure to use along with `cache-dir`.

download-output-no-merge = Do not merge stream
download-output-concat = Merge files using concat
download-output-output = Output filename
download-output-pipe = Pipe to stdout
download-output-pipe-mux = Mux with ffmpeg. Only works when `--pipe` is set.
download-output-pipe-to = Pipe to a file
download-output-experimental-proxy = {"["}Experimental] Provide a M3U8 manifest for other clients by starting a local HTTP Server
download-output-no-recycle = Keep the downloaded segments
