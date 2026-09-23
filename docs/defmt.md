# Logging backends

Two backends, mutually exclusive:

- **`println`** (default) — plain text via `esp-println`. Decoded by any serial monitor.
- **`defmt`** — compact binary frames via `esp-println`'s `defmt-espflash` backend, decoded by
  `espflash --monitor --log-format defmt`.

`defmt` reduces device-to-host transfer overhead, which matters because on a busy channel the
console, not the radio, is usually what saturates first.

## Sync vs async

- With `auto` (the default; every chip except the original ESP32), the backend is chosen at
  runtime from the transport: USB-Serial-JTAG detected → async logging; UART path → sync logging.
  Adding `async-print` does not change that — a UART console is never run through the async
  backend.
- Without `auto`, or on the original ESP32, `async-print` selects async logging and its absence
  selects sync logging.

This keeps the JTAG throughput benefit while preserving UART's low-overhead sync path.

Except on the original ESP32, the crate uses the `USB-JTAG-SERIAL` peripheral, which allows much
higher baud rates than UART.

## Using `defmt` from your own application

The in-repo examples need none of this — the `.cargo/config.toml` aliases (`cargo esp32c3-defmt`)
and `build.rs` handle all three steps. A separate application needs them:

1. **Add `defmt` as a direct dependency.** The `log_ln!` macro expands to `defmt::println!(...)` at
   the call site, so the crate must be resolvable from your code. Plain `defmt = "1.0"` is enough —
   do **not** add `defmt-rtt` or another logger; `esp-println/defmt-espflash` already provides one.
2. **Add `-Tdefmt.x` to your linker flags**, because Cargo does not propagate linker arguments from
   a dependency's build script:
   ```toml
   [target.'cfg(target_arch = "riscv32")']
   rustflags = ["-C", "link-arg=-Tlinkall.x", "-C", "link-arg=-Tdefmt.x"]
   ```
3. **Decode with espflash**: `espflash flash --monitor --log-format defmt <elf>`. No probe-rs or
   J-Link needed — frames stream over the same USB-Serial-JTAG channel as `println!`.

```toml
[dependencies]
# `println` is a default feature and excludes `defmt`, so turn the defaults off and re-add the rest.
esp-csi-rs = { version = "0.11", default-features = false, features = ["esp32c3", "no-std", "auto", "defmt"] }
defmt = "1.0"
```

The selected logging framework must match the one selected for `esp-backtrace`. The `defmt` feature
already pulls the matching `esp-backtrace/defmt`, `esp-hal/defmt` and `esp-radio/defmt` flags.
