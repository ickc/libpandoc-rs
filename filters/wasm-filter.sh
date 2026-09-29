#!/bin/sh
# Run a wasm filter as an ordinary pandoc JSON filter, with wasmtime: link
# or copy this script to NAME beside NAME.wasm, then `pandoc -F NAME`.
# The filter sees the current directory and the environment pandoc gives
# JSON filters. A filter that calls pandoc (parse) needs a host that has
# one: pandocrs's `wasm-filter` instead, `exec wasm-filter "$0.wasm" "$@"`.
exec wasmtime run --dir=. \
	--env PANDOC_VERSION --env PANDOC_READER_OPTIONS \
	--env PANDOC_INPUT_FORMAT --env PANDOC_OUTPUT_FORMAT \
	"$0.wasm" "$@"
