#!/bin/sh
# Run a wasm filter as an ordinary pandoc JSON filter: link or copy this
# script to NAME beside NAME.wasm, then `pandoc -F NAME`.
#
# With pandocrs's `wasm-filter` (`cargo install pandocrs`), as pandocrs runs
# it: the filter sees the current directory, read-only, and may call pandoc.
# Else with `wasmtime run`, which gives directories only read-write: none,
# so the filter reads no files (and can't call pandoc). Either way it gets
# the environment pandoc gives JSON filters, and the limits of
# LIBPANDOC_WASM_TIMEOUT (seconds) and LIBPANDOC_WASM_MAX_MEMORY (bytes, or
# with k, m or g).
if command -v wasm-filter >/dev/null 2>&1; then
	exec wasm-filter "$0.wasm" "$@"
fi
set -- --env PANDOC_VERSION --env PANDOC_READER_OPTIONS \
	--env PANDOC_INPUT_FORMAT --env PANDOC_OUTPUT_FORMAT \
	"$0.wasm" "$@"
if [ -n "${LIBPANDOC_WASM_TIMEOUT:-}" ] && [ "$LIBPANDOC_WASM_TIMEOUT" != 0 ]; then
	ms=$(awk -v t="$LIBPANDOC_WASM_TIMEOUT" 'BEGIN { printf "%d", t * 1000 }')
	set -- -W "timeout=${ms}ms" "$@"
fi
m=${LIBPANDOC_WASM_MAX_MEMORY:-}
case $m in
'' | 0) ;;
*[kK]) set -- -W "max-memory-size=$((${m%?} * 1024))" "$@" ;;
*[mM]) set -- -W "max-memory-size=$((${m%?} * 1048576))" "$@" ;;
*[gG]) set -- -W "max-memory-size=$((${m%?} * 1073741824))" "$@" ;;
*) set -- -W "max-memory-size=$m" "$@" ;;
esac
exec wasmtime run "$@"
