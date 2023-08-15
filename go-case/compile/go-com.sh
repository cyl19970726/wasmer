GOARCH=wasm GOOS=js go build -o main.wasm main.go
wasm2wat -o main.wat main.wasm