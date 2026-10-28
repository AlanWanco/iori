#!/bin/bash
# Rusty-ffmpeg emits native link directives from a dependency build script.
# Cargo uses those directives for the Rust library link, but they do not reach
# the final binary link in this cross-compile setup. Link FFmpeg explicitly here.

# Filter a problematic -lstdc++ argument; it is added in the static block below.
final_args=()
for arg in "$@"; do
  if [[ "$arg" != "-lstdc++" ]]; then
    final_args+=("$arg")
  fi
done

ffmpeg_libs_dir="${FFMPEG_LIBS_DIR:-}"
if [[ -z "$ffmpeg_libs_dir" ]]; then
  # Keep core Windows builds working when the optional FFmpeg feature is off.
  exec x86_64-w64-mingw32-g++ "${final_args[@]}" -static-libgcc -Wl,-Bstatic \
    -lstdc++ -ldbghelp -lgcc_eh -l:libpthread.a -lmsvcrt -lmingwex -lmingw32 \
    -lgcc -lmsvcrt -lmingwex -lkernel32 -Wl,-Bdynamic
fi

ffmpeg_libraries=(avcodec avdevice avfilter avformat avutil swresample swscale)
for library in "${ffmpeg_libraries[@]}"; do
  archive="$ffmpeg_libs_dir/lib${library}.a"
  if [[ ! -f "$archive" ]]; then
    printf 'Missing FFmpeg static library: %s\n' "$archive" >&2
    exit 1
  fi
done

# Append FFmpeg and MinGW static libraries after the Rust objects that use them.
exec x86_64-w64-mingw32-g++ "${final_args[@]}" \
  -static-libgcc \
  -Wl,-Bstatic \
  -L"$ffmpeg_libs_dir" \
  -Wl,--start-group \
  -lavdevice -lavfilter -lavformat -lavcodec -lswscale -lswresample -lavutil \
  -Wl,--end-group \
  -lm -pthread -latomic \
  -lstdc++ -ldbghelp -lgcc_eh -l:libpthread.a -lmsvcrt -lmingwex -lmingw32 \
  -lgcc -lmsvcrt -lmingwex -lkernel32 \
  -Wl,-Bdynamic
