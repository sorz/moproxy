# moproxy-web
Rich web frontend for moproxy

## Dependencies

We use [yew](https://yew.rs) as frontend framework, which requires
`wasm32-unknown-unknown` target, `trunk`, and `wasm-bindgen-cli`.

```bash
rustup target add wasm32-unknown-unknown
cargo install trunk wasm-bindgen-cli
```

## Running

```bash
trunk serve
```

### Build

```bash
trunk build --release
```

The output will be located in the `dist` directory.
