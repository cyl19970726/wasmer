tinygo build -o tiny_main.wasm -target=wasi main.go
wasm2wat -o tiny_main.wat tiny_main.wasm